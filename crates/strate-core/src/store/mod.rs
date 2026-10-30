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
mod audit;
mod github;
mod ingest;
mod live;
mod migrate;
mod model;
pub mod views;

pub use github::{GhRepo, gh_issues, gh_repos, gh_reverts};

use std::path::Path;

use rusqlite::types::Type;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::discovery::Graph;
use crate::hooks::Hook;
use crate::registry::RegistryEvent;
use crate::tail::{Batch, Checkpoint, FileIdentity, Offsets};

pub type Result<T> = rusqlite::Result<T>;

/// The writer connection.
pub struct Store {
    conn: Connection,
}

/// An open write transaction. Dropping it without [`Ingest::commit`] rolls
/// everything in it back.
pub struct Ingest<'a> {
    tx: Transaction<'a>,
}

impl Store {
    /// Opens (or creates) the db at `path` in WAL mode with
    /// `synchronous=NORMAL` and foreign keys on, then applies any pending
    /// migrations and brings the rollups and the agent model up to date.
    /// Audit days are in the machine's zone.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_in(path, &account::system_zone())
    }

    /// [`Store::open`] with audit days in `zone`, an IANA name such as
    /// `America/Toronto`. A store last accounted in another zone has every
    /// rollup recomputed. Fails, opening nothing, on a zone jiff cannot load.
    pub fn open_in(path: impl AsRef<Path>, zone: &str) -> Result<Self> {
        jiff::tz::TimeZone::get(zone).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                Some(format!("unknown time zone {zone:?}: {e}")),
            )
        })?;
        let mut conn = Connection::open(path)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        migrate::migrate(&mut conn, migrate::MIGRATIONS)?;
        let mut store = Self { conn };
        let tx = store.transaction()?;
        // A v1 store holds events but no session starts or /clear links yet.
        if version == 1 {
            ingest::relink_all(&tx.tx)?;
        }
        // A store from before v3 or v6, or accounted with another price list,
        // in another zone or at another rollup version.
        account::refresh(&tx.tx, zone)?;
        // A store from before v21, or built by another model version.
        model::refresh(&tx.tx)?;
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

    /// Stores one batch and its checkpoint in one transaction, with what
    /// it changes (see [`Ingest::batch`]).
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

    /// Applies registry poll events in one transaction: each entry's row in
    /// `registry` (a session it names gets a stub session and orchestrator
    /// until its transcript arrives). The row is the only copy of the
    /// `claude agents` listing's name and state: no agent stores them, and
    /// the views read the row as its process's latest /clear link's. An
    /// entry no longer waiting, in a poll stamped at or after the request,
    /// answers its orchestrator's permission prompt, ending the wait at the
    /// poll; a session no live row lists any more exited, so its prompt
    /// ends when it was last seen. Error events change nothing.
    pub fn apply_registry(&mut self, events: &[RegistryEvent]) -> Result<()> {
        let tx = self.transaction()?;
        for event in events {
            live::registry(&tx.tx, event)?;
        }
        tx.commit()
    }

    /// Applies one hook event: creates the session, orchestrator and agent
    /// rows it names when missing (merging with later discovery and tail
    /// ingest), and sets the agent's `hook_state`. A PermissionRequest opens
    /// a permission wait (a repeat while one is open is the same wait), and
    /// any other hook on the agent ends it, as its next user or assistant
    /// record, or the next `/clear` link's first record, does at that
    /// record's time.
    pub fn apply_hook(&mut self, hook: &Hook) -> Result<()> {
        let tx = self.transaction()?;
        live::hook(&tx.tx, hook)?;
        tx.commit()
    }

    /// Rebuilds the agent model (#107) from the stored rows, in one
    /// transaction: every agent's role and own charge, and the message
    /// edges. Ingest keeps it current on its own, so this is only for a
    /// rebuild; it is idempotent.
    pub fn rebuild_model(&mut self) -> Result<()> {
        let tx = self.transaction()?;
        model::rebuild(&tx.tx)?;
        tx.commit()
    }
}

impl Ingest<'_> {
    /// Writes a batch's events and checkpoint into this transaction, then
    /// relinks the batch's session: its start and its process root's
    /// continuation edges. Then it re-resolves the agents the batch touched
    /// in the agent model, recomputes the cost, time and context rollups of
    /// that session and of any session sharing one of the batch's requests,
    /// and rebuilds the `message` edges when the batch stored a
    /// cross-session message. All of these are functions of the stored rows,
    /// so batches in any order converge to a cold build.
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

/// Each step of `sql`'s query plan, every parameter "".
#[cfg(test)]
pub(crate) fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("plan");
    let params = vec![""; stmt.parameter_count()];
    stmt.query_map(rusqlite::params_from_iter(params), |r| {
        r.get::<_, String>(3)
    })
    .expect("steps")
    .collect::<Result<_>>()
    .expect("detail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tail::test_dir;

    #[test]
    fn a_new_rollup_version_recomputes_every_cell_once() {
        let dir = test_dir("store-v7-upgrade");
        let db = dir.join("strate.db");
        let mut v7 = Connection::open(&db).expect("v7 db");
        migrate::migrate(&mut v7, &migrate::MIGRATIONS[..7]).expect("v7");
        // Accounted at today's prices and zone, by a build before ask_ms.
        v7.execute(
            "INSERT INTO meta (key, value) VALUES ('prices', ?1), ('tz', 'UTC')",
            [crate::cost::PRICES_JSON],
        )
        .expect("meta");
        v7.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
               INSERT INTO agents (session_id, kind) VALUES ('s1', 'orchestrator');
               INSERT INTO events (session_id, type, uuid, timestamp, file, byte_offset, raw,
                       request_key, tool_uses, tool_results, prompt)
               VALUES ('s1', 'user', 'u1', '2026-10-03T10:00:00.000Z', '/p/s1.jsonl', 0,
                       '{"type":"user","message":{"content":"Lorem."}}', NULL, NULL, NULL, 1),
                      ('s1', 'assistant', 'u2', '2026-10-03T10:00:05.000Z', '/p/s1.jsonl', 50,
                       '{"type":"assistant","requestId":"req_1","message":{"content":[
                         {"type":"tool_use","id":"toolu_1","name":"AskUserQuestion"}]}}',
                       'req_1', '[["toolu_1","AskUserQuestion"]]', NULL, NULL),
                      ('s1', 'user', 'u3', '2026-10-03T10:01:05.000Z', '/p/s1.jsonl', 300,
                       '{"type":"user","message":{"content":[
                         {"type":"tool_result","tool_use_id":"toolu_1"}]}}',
                       NULL, NULL, '["toolu_1"]', NULL);
               INSERT INTO audit_cells (session_id, day, wall_ms, active_ms, wait_ms,
                       unpriced, input_tokens, output_tokens, cache_read_tokens,
                       cache_write_tokens)
               VALUES ('s1', '2026-10-03', 65000, 65000, 60000, 0, 0, 0, 0, 0);"#,
        )
        .expect("v7 rows");
        drop(v7);

        let ask = |store: &Store| -> i64 {
            store
                .conn
                .query_row("SELECT sum(ask_ms) FROM audit_cells", [], |r| r.get(0))
                .expect("ask_ms")
        };
        // The question ran 10:00:05 -> 10:01:05.
        let store = Store::open_in(&db, "UTC").expect("upgrade");
        assert_eq!(ask(&store), 60_000);

        // Once recomputed at this version, open leaves the cells alone.
        store
            .conn
            .execute("UPDATE audit_cells SET ask_ms = 99", [])
            .expect("marker");
        drop(store);
        let store = Store::open_in(&db, "UTC").expect("reopen");
        assert_eq!(ask(&store), 99);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_populated_v8_store_reads_its_messages_and_builds_message_edges_on_upgrade() {
        let dir = test_dir("store-v8-upgrade");
        let db = dir.join("strate.db");
        let mut v8 = Connection::open(&db).expect("v8 db");
        migrate::migrate(&mut v8, &migrate::MIGRATIONS[..8]).expect("v8");
        // Fully accounted by the build before rollup 4. The sender has no
        // director's name: any first messager parents a session (#107).
        v8.execute(
            "INSERT INTO meta (key, value) VALUES ('prices', ?1), ('tz', 'UTC'), ('rollup', '3')",
            [crate::cost::PRICES_JSON],
        )
        .expect("meta");
        v8.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('d1', '/p/d1.jsonl'),
                   ('u1', '/p/u1.jsonl');
               INSERT INTO agents (id, session_id, kind) VALUES (1, 'd1', 'orchestrator'),
                   (2, 'u1', 'orchestrator');
               INSERT INTO events (session_id, type, uuid, timestamp, file, byte_offset, raw)
               VALUES ('d1', 'user', 'd1-1', '2026-10-07T10:00:01.000Z', '/p/d1.jsonl', 0,
                       '{"type":"user","message":{"content":[{"type":"tool_result",
                         "tool_use_id":"toolu_1"}]},"toolUseResult":{"msg_id":"m1"}}'),
                      ('u1', 'user', 'u1-1', '2026-10-07T10:00:02.000Z', '/p/u1.jsonl', 0,
                       '{"type":"user","message":{"content":"Lorem."},
                         "origin":{"kind":"peer","msg_id":"m1","name":"lorem"}}');"#,
        )
        .expect("v8 rows");
        drop(v8);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        let edges: Vec<(String, i64, i64)> = store
            .conn
            .prepare("SELECT kind, parent, child FROM edges")
            .expect("edges")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("edges");
        assert_eq!(edges, [("message".into(), 1, 2)]);
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, migrate::MIGRATIONS.len() as i64, "the latest");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v10_store_keeps_an_unanswered_permission_request_of_a_live_session_as_an_open_wait() {
        let dir = test_dir("store-v10-upgrade");
        let db = dir.join("strate.db");
        let mut v10 = Connection::open(&db).expect("v10 db");
        migrate::migrate(&mut v10, &migrate::MIGRATIONS[..10]).expect("v10");
        v10.execute_batch(
            "INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
             INSERT INTO agents (session_id, agent_id, kind, hook_state, hook_waiting_for,
                     hook_at_ms)
             VALUES ('s1', NULL, 'orchestrator', 'needs_you', 'Bash', 1000),
                    ('s1', 'a1', 'subagent', 'running', NULL, 900),
                    ('s1', 'a2', 'subagent', 'needs_you', NULL, 1200);
             -- s2 exited mid-prompt: its registry row is gone.
             INSERT INTO sessions (session_id, path) VALUES ('s2', '/p/s2.jsonl');
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                     first_seen_ms, last_seen_ms, gone)
             VALUES ('pid:1:1', 'interactive', 's2', '/work/x', 1, 'waiting', 500, 1500, 1);
             INSERT INTO agents (session_id, agent_id, kind, hook_state, hook_waiting_for,
                     hook_at_ms)
             VALUES ('s2', NULL, 'orchestrator', 'needs_you', 'Bash', 1100);",
        )
        .expect("v10 rows");
        drop(v10);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        type Wait = (Option<String>, Option<String>, i64, Option<i64>);
        let waits: Vec<Wait> = store
            .conn
            .prepare(
                "SELECT a.agent_id, w.tool_name, w.start_ms, w.end_ms
                 FROM permission_waits AS w JOIN agents AS a ON a.id = w.agent
                 ORDER BY w.start_ms",
            )
            .expect("waits")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("waits");
        assert_eq!(
            waits,
            [
                (None, Some("Bash".into()), 1000, None),
                (Some("a2".into()), None, 1200, None),
            ]
        );
        let gone: Option<String> = store
            .conn
            .query_row(
                "SELECT hook_state FROM agents WHERE session_id = 's2' AND agent_id IS NULL",
                [],
                |r| r.get(0),
            )
            .expect("s2");
        assert_eq!(gone, None, "a gone session needs nothing");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v13_store_drops_the_unwritten_registry_columns_and_keeps_its_agents() {
        let dir = test_dir("store-v13-upgrade");
        let db = dir.join("strate.db");
        let mut v13 = Connection::open(&db).expect("v13 db");
        migrate::migrate(&mut v13, &migrate::MIGRATIONS[..13]).expect("v13");
        // A copy a build before #68 left on the first /clear link.
        v13.execute_batch(
            "INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
             INSERT INTO agents (session_id, kind, name, registry_name, registry_state)
             VALUES ('s1', 'orchestrator', 'lorem', 'ipsum', 'busy');",
        )
        .expect("v13 rows");
        drop(v13);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        let columns: Vec<String> = store
            .conn
            .prepare("SELECT name FROM pragma_table_info('agents') WHERE name LIKE 'registry%'")
            .expect("columns")
            .query_map([], |r| r.get(0))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("names");
        assert_eq!(columns, Vec::<String>::new());
        let name: String = store
            .conn
            .query_row("SELECT name FROM agents WHERE session_id = 's1'", [], |r| {
                r.get(0)
            })
            .expect("s1");
        assert_eq!(name, "lorem");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v18_teammate_typed_by_its_name_reads_teammate_after_upgrade() {
        // A build before #86 stored each teammate's name as its type. One
        // whose meta.json is gone is never rescanned, so the upgrade itself
        // retypes it; a typed teammate and a subagent are left alone.
        let dir = test_dir("store-v18-teammate-type");
        let db = dir.join("strate.db");
        let mut v18 = Connection::open(&db).expect("v18 db");
        migrate::migrate(&mut v18, &migrate::MIGRATIONS[..18]).expect("v18");
        v18.execute_batch(
            "INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
             INSERT INTO agents (session_id, agent_id, kind, name, agent_type) VALUES
                 ('s1', 'a1', 'teammate', 'lorem-mate', 'lorem-mate'),
                 ('s1', 'a2', 'teammate', 'ipsum-mate', 'reviewer'),
                 ('s1', 'a3', 'subagent', 'recon', 'recon');",
        )
        .expect("v18 rows");
        drop(v18);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        let types: Vec<(String, String)> = store
            .conn
            .prepare("SELECT agent_id, agent_type FROM agents ORDER BY agent_id")
            .expect("types")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("types");
        let want = [("a1", "teammate"), ("a2", "reviewer"), ("a3", "recon")];
        assert_eq!(types, want.map(|(a, t)| (a.to_string(), t.to_string())));
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v19_store_drops_its_gear_and_keeps_its_other_rows() {
        // Strate v2 is visibility only (#106): stored gear rows go by design.
        let dir = test_dir("store-v19-drop-gear");
        let db = dir.join("strate.db");
        let mut v19 = Connection::open(&db).expect("v19 db");
        migrate::migrate(&mut v19, &migrate::MIGRATIONS[..19]).expect("v19");
        v19.execute_batch(
            "INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
             INSERT INTO agents (id, session_id, kind, name) VALUES (1, 's1', 'orchestrator', 'lorem');
             INSERT INTO gear_settings (agent, nudge_pct, hard_stop_pct) VALUES (1, 70, 90);",
        )
        .expect("v19 rows");
        drop(v19);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        let gear: Vec<String> = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE name LIKE 'gear%' ORDER BY name")
            .expect("schema")
            .query_map([], |r| r.get(0))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("names");
        assert_eq!(gear, Vec::<String>::new(), "table, view and triggers gone");
        let name: String = store
            .conn
            .query_row("SELECT name FROM agents WHERE session_id = 's1'", [], |r| {
                r.get(0)
            })
            .expect("s1 kept");
        assert_eq!(name, "lorem");
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, migrate::MIGRATIONS.len() as i64, "the latest");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v20_store_gets_its_agent_model_built_on_upgrade_and_keeps_its_edges() {
        let dir = test_dir("store-v20-model");
        let db = dir.join("strate.db");
        let mut v20 = Connection::open(&db).expect("v20 db");
        migrate::migrate(&mut v20, &migrate::MIGRATIONS[..20]).expect("v20");
        // s2's orchestrator received m1, which s1's subagent a1 sent.
        v20.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl'),
                   ('s2', '/p/s2.jsonl');
               INSERT INTO agents (id, session_id, agent_id, kind, name, description) VALUES
                   (1, 's1', NULL, 'orchestrator', 'impl_issue_strate-5', NULL),
                   (2, 's1', 'a1', 'subagent', NULL, 'review_pr_strate-3'),
                   (3, 's2', NULL, 'orchestrator', NULL, NULL);
               INSERT INTO edges (id, kind, parent, child, tool_use_id) VALUES
                   (7, 'dispatch', 1, 2, 'toolu_1');
               INSERT INTO events (session_id, agent_id, type, uuid, timestamp, file,
                       byte_offset, raw)
               VALUES ('s1', 'a1', 'user', 'e1', '2026-10-20T10:00:01.000Z', '/p/a1.jsonl', 0,
                       '{"type":"user","message":{"content":[{"type":"tool_result",
                         "tool_use_id":"toolu_2"}]},"toolUseResult":{"msg_id":"m1"}}'),
                      ('s2', NULL, 'user', 'e2', '2026-10-20T10:00:02.000Z', '/p/s2.jsonl', 0,
                       '{"type":"user","message":{"content":"Lorem."},
                         "origin":{"kind":"peer","msg_id":"m1","name":"lorem"}}');"#,
        )
        .expect("v20 rows");
        drop(v20);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        type Row = (i64, Option<String>, Option<String>, Option<String>);
        let agents: Vec<Row> = store
            .conn
            .prepare(
                "SELECT a.id, a.role, c.key, a.own_charge_source FROM agents AS a
                 LEFT JOIN charges AS c ON c.id = a.own_charge ORDER BY a.id",
            )
            .expect("agents")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("agents");
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            agents,
            [
                (1, some("impl"), some("issue_strate-5"), some("name")),
                (2, some("review"), some("pr_strate-3"), some("name")),
                (3, None, None, None),
            ]
        );
        let edges: Vec<(i64, String, i64, i64, Option<String>)> = store
            .conn
            .prepare("SELECT id, kind, parent, child, tool_use_id FROM edges ORDER BY kind")
            .expect("edges")
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .expect("rows")
            .collect::<Result<_>>()
            .expect("edges");
        assert_eq!(edges.len(), 2, "{edges:?}");
        assert_eq!(
            edges[0],
            (7, "dispatch".into(), 1, 2, some("toolu_1")),
            "kept"
        );
        assert_eq!(
            (edges[1].1.as_str(), edges[1].2, edges[1].3),
            ("message", 2, 3)
        );
        let (holder, source): (i64, String) = store
            .conn
            .query_row(
                "SELECT holder, source FROM agent_charges WHERE agent = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("s2's charge");
        assert_eq!(
            (holder, source.as_str()),
            (2, "parent"),
            "s2 takes a1's charge, by its message"
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

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
    fn an_unknown_zone_is_refused_before_the_db_is_created() {
        let dir = test_dir("store-bad-zone");
        let db = dir.join("strate.db");
        let err = Store::open_in(&db, "Lorem/Ipsum")
            .err()
            .expect("unknown zone");
        assert!(
            matches!(&err, rusqlite::Error::SqliteFailure(_, Some(msg))
                if msg.contains("Lorem/Ipsum")),
            "{err:?}"
        );
        assert!(!db.exists());
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn opening_a_v1_store_backfills_its_events_and_links_its_clear_chains() {
        let dir = test_dir("store-v1-upgrade");
        let db = dir.join("strate.db");
        let mut v1 = Connection::open(&db).expect("v1 db");
        migrate::migrate(&mut v1, &migrate::MIGRATIONS[..1]).expect("v1");
        // s2 is s1's /clear successor: its records name s1 as process root.
        v1.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl'),
                   ('s2', '/p/s2.jsonl');
               INSERT INTO agents (id, session_id, kind) VALUES (1, 's1', 'orchestrator'),
                   (2, 's2', 'orchestrator');
               INSERT INTO events (session_id, type, uuid, timestamp, file, byte_offset, raw)
               VALUES ('s1', 'user', 'u1', '2026-10-02T10:00:00.000Z', '/p/s1.jsonl', 0,
                       '{"type":"user","cwd":"/work/x","gitBranch":"main","session_id":"s1"}'),
                      ('s1', 'custom-title', NULL, NULL, '/p/s1.jsonl', 90,
                       '{"type":"custom-title","customTitle":"demo-1"}'),
                      ('s2', 'user', 'u2', '2026-10-02T11:00:00.000Z', '/p/s2.jsonl', 0,
                       '{"type":"user","cwd":"/work/x","session_id":"s1"}');"#,
        )
        .expect("v1 rows");
        drop(v1);

        let store = Store::open(&db).expect("upgrade");
        let sessions: Vec<(String, String, String)> = store
            .conn
            .prepare("SELECT session_id, process_root, started_at FROM sessions ORDER BY 1")
            .expect("sessions")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("sessions");
        let link = |s: &str, at: &str| (s.to_string(), "s1".to_string(), at.to_string());
        assert_eq!(
            sessions,
            [
                link("s1", "2026-10-02T10:00:00.000Z"),
                link("s2", "2026-10-02T11:00:00.000Z")
            ]
        );
        let (cwd, edge): (String, (i64, i64)) = store
            .conn
            .query_row(
                "SELECT (SELECT cwd FROM events WHERE uuid = 'u1'), parent, child FROM edges
                 WHERE kind = 'continuation'",
                [],
                |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))),
            )
            .expect("linked");
        assert_eq!((cwd.as_str(), edge), ("/work/x", (1, 2)));
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v21_store_drops_its_workstreams_and_keeps_its_rows() {
        let dir = test_dir("store-v21-drop-workstreams");
        let db = dir.join("strate.db");
        let mut v21 = Connection::open(&db).expect("v21 db");
        migrate::migrate(&mut v21, &migrate::MIGRATIONS[..21]).expect("v21");
        // A grouped store: a workstream merged into another, a segment that
        // a subagent and an audit cell point at, and a directs edge.
        v21.execute_batch(
            "INSERT INTO workstreams (id, name) VALUES (1, 'demo-1'), (2, 'demo-2');
             UPDATE workstreams SET merged_into = 1 WHERE id = 2;
             INSERT INTO sessions (session_id, path, workstream_id) VALUES
                 ('s1', '/p/s1.jsonl', 2), ('s2', '/p/s2.jsonl', NULL);
             INSERT INTO segments (id, session_id, start_offset, workstream_id) VALUES
                 (10, 's1', 0, 2);
             INSERT INTO agents (id, session_id, agent_id, kind, segment_id) VALUES
                 (1, 's1', NULL, 'orchestrator', NULL), (2, 's1', 'a1', 'subagent', 10),
                 (3, 's2', NULL, 'orchestrator', NULL);
             INSERT INTO directs (child, parent, msg_id) VALUES ('s2', 's1', 'm1');
             INSERT INTO audit_cells (session_id, agent_id, segment_id, day, wall_ms, active_ms,
                     wait_ms, unpriced, input_tokens, output_tokens, cache_read_tokens,
                     cache_write_tokens)
             VALUES ('s1', 'a1', 10, '2026-10-20', 5, 5, 0, 0, 0, 0, 0, 0);",
        )
        .expect("v21 rows");
        drop(v21);

        let store = Store::open_in(&db, "UTC").expect("upgrade");
        let schema: Vec<String> = store
            .conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE name LIKE '%workstream%' OR name LIKE '%segment%' OR name LIKE 'directs%'
                 ORDER BY name",
            )
            .expect("schema")
            .query_map([], |r| r.get(0))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("names");
        assert_eq!(
            schema,
            Vec::<String>::new(),
            "tables, views and indexes gone"
        );
        let columns: i64 = store
            .conn
            .query_row(
                "SELECT (SELECT count(*) FROM pragma_table_info('agents')
                         WHERE name = 'segment_id')
                     + (SELECT count(*) FROM pragma_table_info('sessions')
                         WHERE name = 'workstream_id')
                     + (SELECT count(*) FROM pragma_table_info('audit_cells')
                         WHERE name = 'segment_id')",
                [],
                |r| r.get(0),
            )
            .expect("columns");
        assert_eq!(columns, 0, "no column names a segment or workstream");
        let agents: i64 = store
            .conn
            .query_row("SELECT count(*) FROM agents", [], |r| r.get(0))
            .expect("agents");
        assert_eq!(agents, 3, "every agent kept");
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, migrate::MIGRATIONS.len() as i64, "the latest");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn charge_costs_sum_each_request_under_its_agents_effective_charge() {
        let dir = test_dir("store-charge-costs");
        let store = Store::open_in(dir.join("strate.db"), "UTC").expect("open");
        // a1 has its own charge; a2 takes its dispatcher a1's; the
        // orchestrator and the request with no agent row are Unlinked. k3
        // streamed twice (its fuller record counts, once); k4 is unpriced.
        store
            .conn
            .execute_batch(
                "INSERT INTO charges (id, key, kind, repo, number) VALUES
                     (7, 'issue_demo-5', 'issue', 'demo', 5);
                 INSERT INTO sessions (session_id) VALUES ('s1');
                 INSERT INTO agents (id, session_id, agent_id, kind, own_charge,
                         own_charge_source) VALUES
                     (1, 's1', NULL, 'orchestrator', NULL, NULL),
                     (2, 's1', 'a1', 'subagent', 7, 'name'),
                     (3, 's1', 'a2', 'subagent', NULL, NULL);
                 INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                     ('dispatch', 2, 3, 't1');
                 INSERT INTO events (session_id, agent_id, file, byte_offset, raw, request_key,
                         usage, output_tokens, cost_usd) VALUES
                     ('s1', NULL, 'p', 0, '{}', 'k1', '{}', 1, 1.0),
                     ('s1', 'a1', 'a1', 0, '{}', 'k2', '{}', 1, 2.0),
                     ('s1', 'a2', 'a2', 0, '{}', 'k3', '{}', 1, 4.0),
                     ('s1', 'a2', 'a2', 10, '{}', 'k3', '{}', 9, 8.0),
                     ('s1', 'a2', 'a2', 20, '{}', 'k4', '{}', 1, NULL),
                     ('s1', 'gone', 'g', 0, '{}', 'k5', '{}', 1, 16.0);",
            )
            .expect("rows");
        let costs: Vec<(Option<i64>, Option<f64>, i64)> = store
            .conn
            .prepare("SELECT charge, cost_usd, unpriced FROM charge_costs ORDER BY charge")
            .expect("costs")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("costs");
        assert_eq!(costs, [(None, Some(17.0), 0), (Some(7), Some(10.0), 1)]);
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

    #[test]
    fn a_pricing_logic_change_alone_reprices_stored_events() {
        let dir = test_dir("store-pricing-logic");
        let db = dir.join("strate.db");
        let mut v2 = Connection::open(&db).expect("v2 db");
        migrate::migrate(&mut v2, &migrate::MIGRATIONS[..2]).expect("v2");
        v2.execute_batch(
            r#"INSERT INTO sessions (session_id, path) VALUES ('s1', '/p/s1.jsonl');
               INSERT INTO agents (session_id, kind) VALUES ('s1', 'orchestrator');
               INSERT INTO events (session_id, type, uuid, timestamp, file, byte_offset, raw)
               VALUES ('s1', 'assistant', 'u2', '2026-10-03T10:00:30.000Z', '/p/s1.jsonl', 0,
                       '{"type":"assistant","requestId":"req_1","message":{"model":"claude-haiku-4-5",
                         "usage":{"input_tokens":1000,"output_tokens":200}}}');"#,
        )
        .expect("v2 rows");
        drop(v2);
        let cost = |store: &Store| -> f64 {
            store
                .conn
                .query_row("SELECT cost_usd FROM events WHERE uuid = 'u2'", [], |r| {
                    r.get(0)
                })
                .expect("cost")
        };
        let store = Store::open(&db).expect("account");
        assert!((cost(&store) - 0.002).abs() < 1e-12);

        // Same prices.json bytes, but accounted by older pricing logic.
        store
            .conn
            .execute_batch(
                "UPDATE meta SET value = 'older' WHERE key = 'pricing_logic';
                 UPDATE events SET cost_usd = 99;",
            )
            .expect("stale logic");
        drop(store);
        let store = Store::open(&db).expect("reopen");
        assert!((cost(&store) - 0.002).abs() < 1e-12, "{}", cost(&store));

        // At the current logic, open leaves the stored $ alone.
        store
            .conn
            .execute("UPDATE events SET cost_usd = 99", [])
            .expect("marker");
        drop(store);
        let store = Store::open(&db).expect("reopen");
        assert_eq!(cost(&store), 99.0);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
