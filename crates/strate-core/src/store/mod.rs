//! The SQLite store: strate's long-term record, which outlives Claude
//! Code's transcript cleanup (`cleanupPeriodDays`).
//!
//! One writer ([`Store`]) and any number of read-only connections
//! ([`Store::open_reader`]) share a WAL-mode db. The store is also the
//! offset store: the consumer of a [`crate::tail::Tailer`] stores each
//! [`Batch`]'s events and its checkpoint in one transaction, and restarts the
//! tailer from [`Store::offsets`]. A crash before commit keeps neither, the
//! batch is re-delivered, and re-ingesting it writes no duplicates (events
//! are unique on `uuid`, else on file + byte offset).

mod ingest;
mod migrate;

use std::path::Path;

use rusqlite::types::Type;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};

use crate::discovery::Graph;
use crate::tail::{Batch, Checkpoint, FileIdentity, Offsets};

pub type Result<T> = rusqlite::Result<T>;

/// The writer connection.
pub struct Store {
    conn: Connection,
}

/// Gear for one agent: context sliders, $ warning, caps and path routing.
/// Written per agent with [`Store::set_gear`] (on the orchestrator: the
/// session's defaults; on any other agent: its overrides, `None`
/// inheriting). Read back resolved by [`Store::gear`], where nudge and hard
/// stop are always set (built-in 80 and 95).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gear {
    pub nudge_pct: Option<u8>,
    pub hard_stop_pct: Option<u8>,
    pub cost_warn_pct: Option<u8>,
    pub cost_cap_usd: Option<f64>,
    pub time_cap_ms: Option<u64>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// An open write transaction. Dropping it without [`Ingest::commit`] rolls
/// everything in it back.
pub struct Ingest<'a> {
    tx: Transaction<'a>,
}

impl Store {
    /// Opens (or creates) the db at `path` in WAL mode with
    /// `synchronous=NORMAL` and foreign keys on, then applies any pending
    /// migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate::migrate(&mut conn, migrate::MIGRATIONS)?;
        Ok(Self { conn })
    }

    /// A read-only connection to the db at `path`. Under WAL it queries
    /// the last committed state while the writer holds a transaction.
    pub fn open_reader(path: impl AsRef<Path>) -> Result<Connection> {
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    /// Every committed checkpoint: where a restarted tailer resumes.
    pub fn offsets(&self) -> Result<Offsets> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, byte_offset, volume, file_index FROM offsets")?;
        let rows = stmt.query_map([], |r| {
            let index: String = r.get(3)?;
            let index = index.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(e))
            })?;
            let checkpoint = Checkpoint {
                offset: r.get::<_, i64>(1)? as u64,
                identity: FileIdentity {
                    volume: r.get::<_, i64>(2)? as u64,
                    index,
                },
            };
            Ok((r.get::<_, String>(0)?.into(), checkpoint))
        })?;
        rows.collect()
    }

    /// Begins a write transaction.
    pub fn transaction(&mut self) -> Result<Ingest<'_>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok(Ingest { tx })
    }

    /// Stores one batch and its checkpoint in one transaction.
    pub fn ingest(&mut self, batch: &Batch) -> Result<()> {
        let tx = self.transaction()?;
        tx.batch(batch)?;
        tx.commit()
    }

    /// Stores a discovery graph's sessions, agents and edges in one
    /// transaction.
    pub fn ingest_graph(&mut self, graph: &Graph) -> Result<()> {
        let tx = self.transaction()?;
        tx.graph(graph)?;
        tx.commit()
    }

    /// Replaces the gear row of one stored agent (`agent_id` `None`: the
    /// session's orchestrator, whose row is the session's defaults). Fails,
    /// changing nothing, when any agent of the session would end up with
    /// its hard stop at or below its nudge.
    pub fn set_gear(
        &mut self,
        session_id: &str,
        agent_id: Option<&str>,
        gear: &Gear,
    ) -> Result<()> {
        let time_cap_ms = gear
            .time_cap_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let changed = self.conn.execute(
            "INSERT INTO gear_settings (agent, nudge_pct, hard_stop_pct, cost_warn_pct,
                 cost_cap_usd, time_cap_ms, cwd, model, effort)
             SELECT id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 FROM agents
             WHERE session_id = ?1 AND agent_id IS ?2
             ON CONFLICT (agent) DO UPDATE SET nudge_pct = excluded.nudge_pct,
                 hard_stop_pct = excluded.hard_stop_pct, cost_warn_pct = excluded.cost_warn_pct,
                 cost_cap_usd = excluded.cost_cap_usd, time_cap_ms = excluded.time_cap_ms,
                 cwd = excluded.cwd, model = excluded.model, effort = excluded.effort",
            params![
                session_id,
                agent_id,
                gear.nudge_pct,
                gear.hard_stop_pct,
                gear.cost_warn_pct,
                gear.cost_cap_usd,
                time_cap_ms,
                gear.cwd,
                gear.model,
                gear.effort,
            ],
        )?;
        match changed {
            0 => Err(rusqlite::Error::QueryReturnedNoRows),
            _ => Ok(()),
        }
    }

    /// The agent's effective gear: its own row, else its orchestrator's,
    /// else the built-in nudge 80 / hard stop 95.
    pub fn gear(&self, session_id: &str, agent_id: Option<&str>) -> Result<Gear> {
        self.conn.query_row(
            "SELECT g.nudge_pct, g.hard_stop_pct, g.cost_warn_pct, g.cost_cap_usd,
                 g.time_cap_ms, g.cwd, g.model, g.effort
             FROM gear_effective AS g JOIN agents AS a ON a.id = g.agent
             WHERE a.session_id = ?1 AND a.agent_id IS ?2",
            params![session_id, agent_id],
            |r| {
                Ok(Gear {
                    nudge_pct: r.get(0)?,
                    hard_stop_pct: r.get(1)?,
                    cost_warn_pct: r.get(2)?,
                    cost_cap_usd: r.get(3)?,
                    time_cap_ms: r.get::<_, Option<i64>>(4)?.map(|ms| ms as u64),
                    cwd: r.get(5)?,
                    model: r.get(6)?,
                    effort: r.get(7)?,
                })
            },
        )
    }
}

impl Ingest<'_> {
    /// Writes a batch's events and checkpoint into this transaction.
    pub fn batch(&self, batch: &Batch) -> Result<()> {
        ingest::batch(&self.tx, batch)
    }

    /// Writes a discovery graph into this transaction.
    pub fn graph(&self, graph: &Graph) -> Result<()> {
        ingest::graph(&self.tx, graph)
    }

    pub fn commit(self) -> Result<()> {
        self.tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tail::test_dir;

    #[test]
    fn the_writer_runs_wal_with_synchronous_normal_and_foreign_keys() {
        let dir = test_dir("store-pragmas");
        let store = Store::open(dir.join("strate.db")).expect("open");
        let pragma = |name: &str| -> String {
            store
                .conn
                .query_row(&format!("PRAGMA {name}"), [], |r| {
                    r.get::<_, rusqlite::types::Value>(0)
                })
                .map(|v| format!("{v:?}"))
                .expect(name)
        };
        assert_eq!(pragma("journal_mode"), "Text(\"wal\")");
        // NORMAL is 1.
        assert_eq!(pragma("synchronous"), "Integer(1)");
        assert_eq!(pragma("foreign_keys"), "Integer(1)");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
