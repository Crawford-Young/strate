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

mod account;
mod group;
mod ingest;
mod live;
mod migrate;
pub mod views;

use std::path::Path;

use rusqlite::types::Type;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};

use crate::discovery::Graph;
use crate::hooks::Hook;
use crate::registry::RegistryEvent;
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
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        migrate::migrate(&mut conn, migrate::MIGRATIONS)?;
        let mut store = Self { conn };
        // A v1 store holds events but no workstream grouping yet.
        if version == 1 {
            store.regroup_all()?;
        }
        // A store from before v3, or priced with another price list.
        let tx = store.transaction()?;
        account::refresh(&tx.tx)?;
        tx.commit()?;
        Ok(store)
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

    /// Stores one batch and its checkpoint in one transaction, regrouping
    /// the batch's session there too (see [`Ingest::batch`]).
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

    /// Applies registry poll events in one transaction: each entry's row in
    /// `registry` (a session it names gets a stub session and orchestrator
    /// until its transcript arrives), and the orchestrator's
    /// `registry_name` and `registry_state`. Error events change nothing.
    pub fn apply_registry(&mut self, events: &[RegistryEvent]) -> Result<()> {
        let tx = self.transaction()?;
        for event in events {
            live::registry(&tx.tx, event)?;
        }
        tx.commit()
    }

    /// Applies one hook event: creates the session, orchestrator and agent
    /// rows it names when missing (merging with later discovery and tail
    /// ingest), and sets the agent's `hook_state`.
    pub fn apply_hook(&mut self, hook: &Hook) -> Result<()> {
        let tx = self.transaction()?;
        live::hook(&tx.tx, hook)?;
        tx.commit()
    }

    /// Rebuilds the workstream grouping of every stored session from the
    /// stored events, in one transaction. Ingest keeps grouping current on
    /// its own, so this is only for a rebuild; it is idempotent and leaves
    /// manual merges alone.
    pub fn regroup_all(&mut self) -> Result<()> {
        let tx = self.transaction()?;
        group::all(&tx.tx)?;
        tx.commit()
    }

    /// Makes workstream `from` read as `into` (and as whatever `into` is
    /// merged into) in `workstream_roots` and `event_workstreams`. The merge
    /// is a pointer on `from`, so it survives regrouping and re-ingest, and
    /// a merged workstream is kept even while no segment uses it. Fails,
    /// changing nothing, when `from` is unknown or the merge would form a
    /// cycle.
    pub fn merge_workstream(&mut self, from: i64, into: i64) -> Result<()> {
        let tx = self.transaction()?;
        let cycle: bool = tx.tx.query_row(
            "WITH RECURSIVE up (id) AS (
                 SELECT ?1 UNION SELECT w.merged_into FROM workstreams AS w
                 JOIN up ON w.id = up.id WHERE w.merged_into IS NOT NULL
             )
             SELECT EXISTS (SELECT 1 FROM up WHERE id = ?2)",
            params![into, from],
            |r| r.get(0),
        )?;
        if cycle {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some(format!("workstream {from} into {into} would form a cycle")),
            ));
        }
        let changed = tx.tx.execute(
            "UPDATE workstreams SET merged_into = ?2 WHERE id = ?1",
            params![from, into],
        )?;
        match changed {
            0 => Err(rusqlite::Error::QueryReturnedNoRows),
            _ => tx.commit(),
        }
    }

    /// Undoes [`Store::merge_workstream`] on `id`; workstreams merged into
    /// `id` stay merged into it. Fails when `id` is unknown.
    pub fn unmerge_workstream(&mut self, id: i64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE workstreams SET merged_into = NULL WHERE id = ?1",
            [id],
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
    /// Writes a batch's events and checkpoint into this transaction, then
    /// regroups the batch's session: its name segments and workstream, its
    /// subagents' segments, and its process root's continuation edges.
    /// Then it recomputes the cost, time and context rollups of that
    /// session and of any session sharing one of the batch's requests.
    /// Grouping and rollups are functions of the stored events, so batches
    /// in any order converge to a cold build. A subagent that only a graph
    /// has added gets its segment at its session's next batch.
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

    #[test]
    fn opening_a_v1_store_backfills_and_groups_its_events() {
        let dir = test_dir("store-v1-upgrade");
        let db = dir.join("strate.db");
        let mut v1 = Connection::open(&db).expect("v1 db");
        migrate::migrate(&mut v1, &migrate::MIGRATIONS[..1]).expect("v1");
        v1.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
               INSERT INTO agents (session_id, kind) VALUES ('s1', 'orchestrator');
               INSERT INTO events (session_id, type, uuid, timestamp, file, byte_offset, raw)
               VALUES ('s1', 'user', 'u1', '2026-10-02T10:00:00.000Z', '/p/s1.jsonl', 0,
                       '{"type":"user","cwd":"/work/x","gitBranch":"main","session_id":"root"}'),
                      ('s1', 'custom-title', NULL, NULL, '/p/s1.jsonl', 90,
                       '{"type":"custom-title","customTitle":"demo-1"}');"#,
        )
        .expect("v1 rows");
        drop(v1);

        let store = Store::open(&db).expect("upgrade");
        let row: (String, String, String) = store
            .conn
            .query_row(
                "SELECT w.name, s.process_root, e.cwd FROM sessions AS s
                 JOIN workstreams AS w ON w.id = s.workstream_id
                 JOIN events AS e ON e.uuid = 'u1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("grouped");
        assert_eq!(row, ("demo-1".into(), "root".into(), "/work/x".into()));
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn opening_a_v2_store_accounts_it_and_a_new_price_list_reaccounts() {
        let dir = test_dir("store-v2-upgrade");
        let db = dir.join("strate.db");
        let mut v2 = Connection::open(&db).expect("v2 db");
        migrate::migrate(&mut v2, &migrate::MIGRATIONS[..2]).expect("v2");
        v2.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
               INSERT INTO agents (session_id, kind) VALUES ('s1', 'orchestrator');
               INSERT INTO events (session_id, type, uuid, timestamp, file, byte_offset, raw)
               VALUES ('s1', 'user', 'u1', '2026-10-03T10:00:00.000Z', '/p/s1.jsonl', 0,
                       '{"type":"user","message":{"content":"Lorem."}}'),
                      ('s1', 'assistant', 'u2', '2026-10-03T10:00:30.000Z', '/p/s1.jsonl', 50,
                       '{"type":"assistant","requestId":"req_1","message":{"model":"claude-haiku-4-5",
                         "usage":{"input_tokens":1000,"output_tokens":200}}}');"#,
        )
        .expect("v2 rows");
        drop(v2);

        let rollup = |store: &Store| -> (f64, i64, i64) {
            store
                .conn
                .query_row(
                    "SELECT s.cost_usd, s.run_ms, a.context_tokens FROM sessions AS s
                     JOIN agents AS a ON a.session_id = s.session_id",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .expect("rollup")
        };
        let store = Store::open(&db).expect("upgrade");
        // haiku: (1000 * 1 + 200 * 5) / 1e6 = $0.002; records 30 s apart;
        // depth 1000 input.
        let (usd, run_ms, depth) = rollup(&store);
        assert!((usd - 0.002).abs() < 1e-12, "{usd}");
        assert_eq!((run_ms, depth), (30_000, 1000));

        // Priced with another list: open re-accounts.
        store
            .conn
            .execute_batch(
                "UPDATE meta SET value = 'stale' WHERE key = 'prices';
                 UPDATE events SET cost_usd = 99 WHERE cost_usd IS NOT NULL;
                 UPDATE sessions SET cost_usd = 99;",
            )
            .expect("stale");
        drop(store);
        let store = Store::open(&db).expect("reopen");
        assert!((rollup(&store).0 - 0.002).abs() < 1e-12);

        // At the current list, open leaves the rollups alone.
        store
            .conn
            .execute("UPDATE sessions SET cost_usd = 99", [])
            .expect("marker");
        drop(store);
        let store = Store::open(&db).expect("reopen");
        assert_eq!(rollup(&store).0, 99.0);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
