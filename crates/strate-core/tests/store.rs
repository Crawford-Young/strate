//! Integration tests for the SQLite store: schema, transactional offsets,
//! ingest of the synthetic #10 fixture, survival past transcript deletion,
//! WAL concurrency and gear settings. Every db lives in a temp dir.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};
use serde_json::json;
use strate_core::discovery::discover;
use strate_core::store::{Gear, Store};
use strate_core::tail::{
    Batch, Checkpoint, FileIdentity, Offsets, ResetReason, TailEvent, TailOptions, Tailer,
};

const PATIENCE: Duration = Duration::from_secs(10);
const SA: &str = "5e55a0a0-0000-4000-8000-00000000000a";
const SB: &str = "5e55b0b0-0000-4000-8000-00000000000b";
const LONG_DIR: &str = "-work--worktrees-demo-repo-7-lorem-ipsum-dolor-sit-x7k2q9";

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("strate.db")
    }

    /// A copy of the synthetic fixture config dir, safe to delete from.
    fn fixture(&self) -> PathBuf {
        let root = self.0.join("claude-home");
        copy_dir(&fixture_root(), &root);
        root
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("mkdir");
    for entry in fs::read_dir(from).expect("read dir") {
        let path = entry.expect("entry").path();
        let dest = to.join(path.file_name().expect("name"));
        if path.is_dir() {
            copy_dir(&path, &dest);
        } else {
            fs::copy(&path, &dest).expect("copy");
        }
    }
}

/// Every transcript under `root/projects`, found by walking the tree
/// rather than through the crate.
fn transcripts(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("readable dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("projects"), &mut out);
    out.retain(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        name.ends_with(".jsonl") && !p.components().any(|c| c.as_os_str() == "tool-results")
    });
    out
}

/// Where each transcript's checkpoint must end: just past its last newline.
fn caught_up(root: &Path) -> Offsets {
    transcripts(root)
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).expect("read");
            let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            let identity = FileIdentity::of(&path).expect("identity");
            (
                path,
                Checkpoint {
                    offset: end as u64,
                    identity,
                },
            )
        })
        .collect()
}

/// Tails `root` from `start` and hands every batch to `on_batch` until
/// every transcript's delivered checkpoint reaches its last newline.
/// Returns the batches.
fn tail_all(root: &Path, start: Offsets, mut on_batch: impl FnMut(&Batch)) -> Vec<Batch> {
    let target = caught_up(root);
    let mut delivered = start.clone();
    let options = TailOptions {
        rescan_every: Duration::from_secs(3600),
        ..TailOptions::default()
    };
    let (tailer, rx) = Tailer::start(root, start, options).expect("start");
    let deadline = Instant::now() + PATIENCE;
    let mut batches = Vec::new();
    while target.iter().any(|(p, c)| delivered.get(p) != Some(c)) {
        let left = deadline.saturating_duration_since(Instant::now());
        let batch = rx
            .recv_timeout(left)
            .unwrap_or_else(|e| panic!("{e}; delivered {delivered:?}"));
        on_batch(&batch);
        if let Some(c) = batch.checkpoint {
            delivered.insert(batch.path.to_path_buf(), c);
        }
        batches.push(batch);
    }
    tailer.stop();
    batches
}

/// Discovers `root` and ingests the graph plus every tail batch from the
/// store's own offsets.
fn ingest_root(store: &mut Store, root: &Path) {
    let graph = discover(root).expect("discover");
    store.ingest_graph(&graph).expect("ingest graph");
    let offsets = store.offsets().expect("offsets");
    tail_all(root, offsets, |b| store.ingest(b).expect("ingest batch"));
}

fn reader(db: &Path) -> Connection {
    Store::open_reader(db).expect("reader")
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

/// Row counts of every ingested table, for idempotency checks.
fn counts(conn: &Connection) -> Vec<(&'static str, i64)> {
    [
        "events",
        "sessions",
        "agents",
        "edges",
        "offsets",
        "github_links",
    ]
    .into_iter()
    .map(|t| (t, count(conn, t)))
    .collect()
}

fn columns(conn: &Connection, table: &str) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .expect("prepare");
    stmt.query_map([], |r| r.get(0))
        .expect("query")
        .map(|c| c.expect("column"))
        .collect()
}

fn record(path: &Arc<Path>, offset: u64, value: serde_json::Value) -> TailEvent {
    TailEvent::Record {
        path: path.clone(),
        offset,
        value,
    }
}

fn batch(path: &Arc<Path>, events: Vec<TailEvent>, offset: u64) -> Batch {
    Batch {
        path: path.clone(),
        events,
        checkpoint: Some(Checkpoint {
            offset,
            identity: FileIdentity {
                volume: u64::MAX,
                index: u128::MAX,
            },
        }),
    }
}

fn session_path(dir: &Path, session: &str) -> Arc<Path> {
    dir.join(format!("claude/projects/-work-x/{session}.jsonl"))
        .into()
}

// ---- schema -------------------------------------------------------------

#[test]
fn open_creates_every_table_with_the_required_columns() {
    let tmp = Temp::new("schema");
    let store = Store::open(tmp.db()).expect("open");
    drop(store);
    let conn = reader(&tmp.db());
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert!(version >= 1, "migrated: v{version}");

    let required: &[(&str, &[&str])] = &[
        (
            "events",
            &[
                "session_id",
                "agent_id",
                "type",
                "subtype",
                "uuid",
                "parent_uuid",
                "request_id",
                "timestamp",
                "file",
                "byte_offset",
                "raw",
            ],
        ),
        (
            "sessions",
            &[
                "session_id",
                "project_dir",
                "path",
                "stub",
                "workstream_id",
                "cost_usd",
                "run_ms",
            ],
        ),
        (
            "agents",
            &[
                "session_id",
                "agent_id",
                "kind",
                "agent_type",
                "description",
                "spawn_depth",
                "model",
                "effort",
                "cwd",
                "git_branch",
                "version",
                "name",
                "registry_name",
                "registry_state",
                "cost_usd",
                "run_ms",
            ],
        ),
        (
            "edges",
            &["kind", "parent", "child", "tool_use_id", "team_name"],
        ),
        ("workstreams", &["name"]),
        ("offsets", &["path", "byte_offset", "volume", "file_index"]),
        (
            "intents",
            &[
                "parent_agent",
                "cwd",
                "model",
                "effort",
                "cost_cap_usd",
                "time_cap_ms",
                "status",
                "created_at",
                "correlated_agent",
            ],
        ),
        (
            "gear_settings",
            &[
                "agent",
                "nudge_pct",
                "hard_stop_pct",
                "cost_warn_pct",
                "cost_cap_usd",
                "time_cap_ms",
                "cwd",
                "model",
                "effort",
            ],
        ),
        (
            "github_links",
            &["session_id", "pr_number", "pr_url", "pr_repository"],
        ),
    ];
    for (table, wanted) in required {
        let have = columns(&conn, table);
        for column in *wanted {
            assert!(have.contains(*column), "{table}.{column} missing: {have:?}");
        }
    }
}

#[test]
fn reopening_an_existing_store_keeps_its_rows_and_version() {
    let tmp = Temp::new("reopen");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .ingest(&batch(
            &path,
            vec![record(&path, 0, json!({"type": "user", "uuid": "u1"}))],
            10,
        ))
        .expect("ingest");
    drop(store);
    let version = |db: &Path| -> i64 {
        reader(db)
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version")
    };
    let before = version(&tmp.db());

    let store = Store::open(tmp.db()).expect("reopen");
    assert_eq!(version(&tmp.db()), before);
    assert_eq!(count(&reader(&tmp.db()), "events"), 1);
    assert_eq!(
        store
            .offsets()
            .expect("offsets")
            .get(&*path)
            .map(|c| c.offset),
        Some(10)
    );
}

#[test]
fn intents_take_defaults_and_reference_agents() {
    let tmp = Temp::new("intents");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .ingest(&batch(
            &path,
            vec![record(&path, 0, json!({"type": "user"}))],
            5,
        ))
        .expect("ingest");
    drop(store);

    let conn = Connection::open(tmp.db()).expect("writer");
    conn.pragma_update(None, "foreign_keys", true)
        .expect("fk on");
    conn.execute(
        "INSERT INTO intents (parent_agent, cwd, model, effort, cost_cap_usd, time_cap_ms)
         SELECT id, '/work/x', 'claude-opus-5-5', 'high', 5.0, 600000 FROM agents
         WHERE session_id = 's1' AND agent_id IS NULL",
        [],
    )
    .expect("insert intent");
    let (status, created, correlated): (String, String, Option<i64>) = conn
        .query_row(
            "SELECT status, created_at, correlated_agent FROM intents",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("intent");
    assert_eq!(status, "pending");
    assert!(created.starts_with("20"), "{created}");
    assert_eq!(correlated, None);
    assert!(
        conn.execute("INSERT INTO intents (parent_agent) VALUES (9999)", [])
            .is_err(),
        "an intent's parent must be a stored agent"
    );
}

// ---- WAL and concurrency ------------------------------------------------

#[test]
fn wal_is_on_and_a_reader_queries_while_the_writer_holds_a_write_txn() {
    let tmp = Temp::new("wal");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    let conn = reader(&tmp.db());
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal mode");
    assert_eq!(mode, "wal");

    let txn = store.transaction().expect("begin");
    txn.batch(&batch(
        &path,
        vec![record(&path, 0, json!({"type": "user", "uuid": "u1"}))],
        8,
    ))
    .expect("write inside the open txn");

    // The writer holds its write lock; the reader neither blocks nor sees
    // the uncommitted rows.
    let started = Instant::now();
    assert_eq!(count(&conn, "events"), 0);
    assert!(started.elapsed() < Duration::from_secs(1), "reader blocked");

    txn.commit().expect("commit");
    assert_eq!(count(&conn, "events"), 1);
    assert!(
        conn.execute("DELETE FROM events", []).is_err(),
        "reader is read-only"
    );
}

// ---- transactional offsets ----------------------------------------------

#[test]
fn a_batch_and_its_checkpoint_commit_together() {
    let tmp = Temp::new("together");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    let b = Batch {
        checkpoint: Some(Checkpoint {
            offset: 42,
            identity: FileIdentity {
                volume: u64::MAX - 3,
                index: u128::MAX - 5,
            },
        }),
        ..batch(&path, vec![record(&path, 0, json!({"type": "user"}))], 0)
    };
    store.ingest(&b).expect("ingest");
    assert_eq!(
        store.offsets().expect("offsets").get(&*path),
        b.checkpoint.as_ref(),
        "u64 volume and u128 index round-trip"
    );
    assert_eq!(count(&reader(&tmp.db()), "events"), 1);
}

#[test]
fn a_rolled_back_batch_stores_neither_events_nor_checkpoint() {
    let tmp = Temp::new("rollback");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    let txn = store.transaction().expect("begin");
    txn.batch(&batch(
        &path,
        vec![record(&path, 0, json!({"type": "user"}))],
        9,
    ))
    .expect("write");
    drop(txn); // dies before commit

    assert_eq!(store.offsets().expect("offsets"), Offsets::new());
    assert_eq!(count(&reader(&tmp.db()), "events"), 0);
}

#[test]
fn crash_before_commit_re_delivers_the_batch_and_the_replay_adds_no_duplicates() {
    let tmp = Temp::new("crash");
    let root = tmp.0.join("claude");
    let file = root.join("projects/-work-x/s1.jsonl");
    fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
    // Records with and without uuid, so both idempotency keys are exercised.
    let text: String = (0..6)
        .map(|n| {
            if n % 2 == 0 {
                format!("{{\"type\":\"user\",\"uuid\":\"u{n}\",\"n\":{n}}}\n")
            } else {
                format!("{{\"type\":\"cost-state\",\"n\":{n}}}\n")
            }
        })
        .collect();
    fs::write(&file, &text).expect("write");
    let options = TailOptions {
        batch_records: 2,
        rescan_every: Duration::from_secs(3600),
        ..TailOptions::default()
    };

    // Run 1: the first batch commits; the second is written inside a txn
    // that never commits, then everything goes away as in a crash.
    let mut store = Store::open(tmp.db()).expect("open");
    let (tailer, rx) = Tailer::start(&root, Offsets::new(), options.clone()).expect("start");
    let first = rx.recv_timeout(PATIENCE).expect("first batch");
    store.ingest(&first).expect("commit first");
    let second = rx.recv_timeout(PATIENCE).expect("second batch");
    let txn = store.transaction().expect("begin");
    txn.batch(&second).expect("write second");
    drop(txn);
    drop(tailer);
    drop(rx);
    drop(store);

    // Run 2: the store resumes after the first batch only.
    let mut store = Store::open(tmp.db()).expect("reopen");
    let offsets = store.offsets().expect("offsets");
    assert_eq!(offsets.get(&file), first.checkpoint.as_ref());
    assert_eq!(count(&reader(&tmp.db()), "events"), 2);
    let (tailer, rx) = Tailer::start(&root, offsets, options).expect("restart");
    let mut redelivered = Vec::new();
    while redelivered.len() < 4 {
        let b = rx.recv_timeout(PATIENCE).expect("redelivered batch");
        store.ingest(&b).expect("ingest");
        redelivered.extend(b.events);
    }
    tailer.stop();
    assert_eq!(
        redelivered[..2],
        second.events[..],
        "the lost batch comes back first"
    );
    assert_eq!(count(&reader(&tmp.db()), "events"), 6);

    // Replaying an already-committed batch writes nothing new.
    store.ingest(&second).expect("replay");
    store.ingest(&first).expect("replay");
    assert_eq!(count(&reader(&tmp.db()), "events"), 6);
    let ns: Vec<i64> = {
        let conn = reader(&tmp.db());
        let mut stmt = conn
            .prepare("SELECT raw ->> '$.n' FROM events ORDER BY byte_offset")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .map(|n| n.expect("n"))
            .collect()
    };
    assert_eq!(ns, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn a_uuid_replayed_into_another_file_is_stored_once() {
    let tmp = Temp::new("uuid");
    let (a, b) = (session_path(&tmp.0, "s1"), session_path(&tmp.0, "s2"));
    let mut store = Store::open(tmp.db()).expect("open");
    let rec = json!({"type": "user", "uuid": "u-same", "cwd": "/work/old"});
    store
        .ingest(&batch(&a, vec![record(&a, 0, rec.clone())], 30))
        .expect("ingest a");
    // A resumed session replays the record into its own file.
    let own = json!({"type": "user", "uuid": "u-own", "cwd": "/work/new"});
    store
        .ingest(&batch(
            &b,
            vec![record(&b, 0, own), record(&b, 100, rec)],
            130,
        ))
        .expect("ingest b");
    let conn = reader(&tmp.db());
    assert_eq!(count(&conn, "events"), 2);
    assert_eq!(
        agent_text(&conn, "s2", None, "cwd").as_deref(),
        Some("/work/new"),
        "a replayed record is not re-applied to the agent"
    );
    let file: String = conn
        .query_row("SELECT file FROM events", [], |r| r.get(0))
        .expect("event");
    assert_eq!(Path::new(&file), &*a, "the first sighting is kept");
}

#[test]
fn reset_drops_the_files_uuidless_events_so_the_rewrite_is_stored() {
    let tmp = Temp::new("reset");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .ingest(&batch(
            &path,
            vec![
                record(&path, 0, json!({"type": "ai-title", "aiTitle": "old"})),
                record(&path, 30, json!({"type": "user", "uuid": "u1"})),
            ],
            60,
        ))
        .expect("first read");
    store
        .ingest(&batch(
            &path,
            vec![
                TailEvent::Reset {
                    path: path.clone(),
                    reason: ResetReason::Replaced,
                },
                record(&path, 0, json!({"type": "ai-title", "aiTitle": "new"})),
                record(&path, 30, json!({"type": "user", "uuid": "u1"})),
            ],
            60,
        ))
        .expect("re-read");
    let conn = reader(&tmp.db());
    let titles: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT raw ->> '$.aiTitle' FROM events WHERE type = 'ai-title'")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .map(|t| t.expect("title"))
            .collect()
    };
    assert_eq!(titles, vec!["new".to_string()]);
    assert_eq!(count(&conn, "events"), 2);
}

#[test]
fn malformed_and_error_events_store_no_rows_but_the_checkpoint_moves() {
    let tmp = Temp::new("malformed");
    let path = session_path(&tmp.0, "s1");
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .ingest(&batch(
            &path,
            vec![TailEvent::Malformed {
                path: path.clone(),
                offset: 0,
                error: "eof".into(),
            }],
            12,
        ))
        .expect("ingest");
    store
        .ingest(&Batch {
            path: path.clone(),
            events: vec![TailEvent::Error {
                path: path.clone(),
                message: "denied".into(),
            }],
            checkpoint: None,
        })
        .expect("ingest error");
    assert_eq!(count(&reader(&tmp.db()), "events"), 0);
    assert_eq!(
        store
            .offsets()
            .expect("offsets")
            .get(&*path)
            .map(|c| c.offset),
        Some(12)
    );
}

// ---- fixture ingest -----------------------------------------------------

/// (agent_id or "orchestrator", kind) per agent of `session`.
fn agents_of(conn: &Connection, session: &str) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT ifnull(agent_id, 'orchestrator'), kind FROM agents
             WHERE session_id = ?1 ORDER BY agent_id",
        )
        .expect("prepare");
    stmt.query_map([session], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|a| a.expect("agent"))
        .collect()
}

/// (kind, parent agent id or "orchestrator"/"-", child agent id, tool_use_id
/// or team) for every edge.
fn edges(conn: &Connection) -> BTreeSet<(String, String, String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT e.kind,
                    CASE WHEN p.id IS NULL THEN '-' ELSE ifnull(p.agent_id, 'orchestrator:' || p.session_id) END,
                    c.agent_id,
                    ifnull(e.tool_use_id, e.team_name)
             FROM edges e JOIN agents c ON c.id = e.child LEFT JOIN agents p ON p.id = e.parent",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .map(|e| e.expect("edge"))
        .collect()
}

fn agent_text(
    conn: &Connection,
    session: &str,
    agent: Option<&str>,
    column: &str,
) -> Option<String> {
    conn.query_row(
        &format!("SELECT {column} FROM agents WHERE session_id = ?1 AND agent_id IS ?2"),
        params![session, agent],
        |r| r.get(0),
    )
    .expect("agent row")
}

#[test]
fn fixture_ingest_produces_the_expected_sessions_agents_and_edges() {
    let tmp = Temp::new("fixture");
    let root = tmp.fixture();
    let mut store = Store::open(tmp.db()).expect("open");
    ingest_root(&mut store, &root);
    let conn = reader(&tmp.db());

    let sessions: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT session_id, project_dir, stub FROM sessions ORDER BY session_id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .map(|s| s.expect("session"))
            .collect()
    };
    assert_eq!(
        sessions,
        vec![
            (SA.to_string(), "-work-demo-repo".to_string(), 0),
            (SB.to_string(), LONG_DIR.to_string(), 0),
        ]
    );

    let s = |a: &str, k: &str| (a.to_string(), k.to_string());
    assert_eq!(
        agents_of(&conn, SA),
        vec![
            s("orchestrator", "orchestrator"),
            s("a1000000000000001", "subagent"),
            s("a1000000000000002", "subagent"),
            s("a1000000000000003", "subagent"),
            s("a1000000000000004", "subagent"),
            s("a1000000000000005", "subagent"),
            s("a1000000000000006", "teammate"),
        ]
    );
    assert_eq!(
        agents_of(&conn, SB),
        vec![
            s("orchestrator", "orchestrator"),
            s("a2000000000000001", "subagent")
        ]
    );

    let e = |k: &str, p: &str, c: &str, via: &str| {
        (k.to_string(), p.to_string(), c.to_string(), via.to_string())
    };
    let orch_a = format!("orchestrator:{SA}");
    let orch_b = format!("orchestrator:{SB}");
    assert_eq!(
        edges(&conn),
        BTreeSet::from([
            e(
                "dispatch",
                &orch_a,
                "a1000000000000001",
                "toolu_01AAAAAAAAAAAAAAAAAAAAAA"
            ),
            e(
                "dispatch",
                &orch_a,
                "a1000000000000002",
                "toolu_01BBBBBBBBBBBBBBBBBBBBBB"
            ),
            e(
                "dispatch",
                "a1000000000000001",
                "a1000000000000003",
                "toolu_01CCCCCCCCCCCCCCCCCCCCCC"
            ),
            e(
                "dispatch",
                &orch_b,
                "a2000000000000001",
                "toolu_02AAAAAAAAAAAAAAAAAAAAAA"
            ),
            // Team demo-team's lead session is SA.
            e("teammate", &orch_a, "a1000000000000006", "demo-team"),
        ]),
        "no edge for the meta-less a…04 or the unresolvable a…05"
    );

    // Meta fields, with transcript values (model, cwd, branch) on top.
    let a1 = Some("a1000000000000001");
    assert_eq!(
        agent_text(&conn, SA, a1, "agent_type").as_deref(),
        Some("implementer")
    );
    assert_eq!(agent_text(&conn, SA, a1, "effort").as_deref(), Some("high"));
    assert_eq!(
        agent_text(&conn, SA, a1, "cwd").as_deref(),
        Some("/work/demo-repo")
    );
    assert_eq!(
        agent_text(&conn, SA, a1, "git_branch").as_deref(),
        Some("main")
    );
    let a3 = Some("a1000000000000003");
    assert_eq!(
        agent_text(&conn, SA, a3, "model").as_deref(),
        Some("claude-sonnet-5"),
        "no meta model: the transcript's message.model fills it"
    );
    let depth: i64 = conn
        .query_row(
            "SELECT spawn_depth FROM agents WHERE agent_id = 'a1000000000000003'",
            [],
            |r| r.get(0),
        )
        .expect("depth");
    assert_eq!(depth, 2);
    assert_eq!(
        agent_text(&conn, SA, Some("a1000000000000006"), "name").as_deref(),
        Some("demo-reviewer")
    );
    assert_eq!(
        agent_text(&conn, SA, Some("a1000000000000005"), "unresolved").as_deref(),
        Some("tool_use_not_found")
    );

    // Orchestrator B: envelope cwd/branch/version, latest /rename name,
    // model and effort from its assistant records.
    assert_eq!(
        agent_text(&conn, SB, None, "cwd").as_deref(),
        Some("/work/.worktrees/demo-repo-7")
    );
    assert_eq!(
        agent_text(&conn, SB, None, "git_branch").as_deref(),
        Some("feat/7-lorem")
    );
    assert_eq!(
        agent_text(&conn, SB, None, "version").as_deref(),
        Some("2.1.263")
    );
    assert_eq!(
        agent_text(&conn, SB, None, "name").as_deref(),
        Some("demo-repo-7")
    );
    assert_eq!(
        agent_text(&conn, SB, None, "model").as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        agent_text(&conn, SB, None, "effort").as_deref(),
        Some("medium")
    );
    assert_eq!(
        agent_text(&conn, SA, None, "name"),
        None,
        "SA was never renamed"
    );

    let link: (String, i64, String, String) = conn
        .query_row(
            "SELECT session_id, pr_number, pr_url, pr_repository FROM github_links",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("one link");
    assert_eq!(
        link,
        (
            SB.to_string(),
            7,
            "https://github.com/example/demo-repo/pull/7".to_string(),
            "example/demo-repo".to_string()
        )
    );

    // Indexed event columns: the orchestrator's records have no agent_id,
    // a subagent's carry it; uuid/parent/request/timestamp are lifted out.
    let (agent, parent, request, ts): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT agent_id, parent_uuid, request_id, timestamp FROM events WHERE uuid = 'u-a003'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("u-a003");
    assert_eq!(agent, None);
    assert_eq!(parent.as_deref(), Some("u-a002"));
    assert_eq!(request.as_deref(), Some("req_01"));
    assert_eq!(ts.as_deref(), Some("2026-10-01T10:00:00.000Z"));
    let sub: (String, String) = conn
        .query_row(
            "SELECT session_id, agent_id FROM events WHERE uuid = 'a1000000000000002-u1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("subagent event");
    assert_eq!(sub, (SA.to_string(), "a1000000000000002".to_string()));
    let subtype: String = conn
        .query_row(
            "SELECT subtype FROM events WHERE uuid = 'u-a006'",
            [],
            |r| r.get(0),
        )
        .expect("system event");
    assert_eq!(subtype, "turn_duration");
}

#[test]
fn re_ingesting_the_fixture_is_idempotent() {
    let tmp = Temp::new("idempotent");
    let root = tmp.fixture();
    let mut store = Store::open(tmp.db()).expect("open");
    ingest_root(&mut store, &root);
    let conn = reader(&tmp.db());
    let first = counts(&conn);
    let delivered: usize = tail_all(&root, Offsets::new(), |_| {})
        .iter()
        .flat_map(|b| &b.events)
        .filter(|e| matches!(e, TailEvent::Record { .. }))
        .count();
    assert_eq!(first[0], ("events", delivered as i64), "one row per record");

    // Same graph again, and every batch re-delivered from offset 0.
    let graph = discover(&root).expect("discover");
    store.ingest_graph(&graph).expect("graph again");
    tail_all(&root, Offsets::new(), |b| store.ingest(b).expect("replay"));
    assert_eq!(counts(&conn), first);
}

#[test]
fn rows_survive_transcript_deletion() {
    let tmp = Temp::new("survive");
    let root = tmp.fixture();
    let mut store = Store::open(tmp.db()).expect("open");
    ingest_root(&mut store, &root);
    let conn = reader(&tmp.db());
    let before = counts(&conn);

    // cleanupPeriodDays removes session A entirely and session B's jsonl.
    let project_a = root.join("projects/-work-demo-repo");
    fs::remove_dir_all(&project_a).expect("remove A");
    fs::remove_file(root.join(format!("projects/{LONG_DIR}/{SB}.jsonl"))).expect("remove B");
    ingest_root(&mut store, &root);

    assert_eq!(counts(&conn), before);
    let stub: i64 = conn
        .query_row(
            "SELECT stub FROM sessions WHERE session_id = ?1",
            [SB],
            |r| r.get(0),
        )
        .expect("B");
    assert_eq!(stub, 0, "B's transcript was seen, so it is no stub");
    assert_eq!(
        edges(&conn).len(),
        5,
        "edges outlive the tool_use that made them"
    );
    assert_eq!(
        agent_text(&conn, SB, Some("a2000000000000001"), "unresolved"),
        None,
        "its stored edge still resolves it"
    );
}

#[test]
fn subagents_whose_session_file_is_gone_get_a_stub_session() {
    let tmp = Temp::new("stub");
    let root = tmp.fixture();
    fs::remove_file(root.join(format!("projects/{LONG_DIR}/{SB}.jsonl"))).expect("remove B");
    let mut store = Store::open(tmp.db()).expect("open");
    ingest_root(&mut store, &root);
    let conn = reader(&tmp.db());

    let (stub, project, path): (i64, String, Option<String>) = conn
        .query_row(
            "SELECT stub, project_dir, path FROM sessions WHERE session_id = ?1",
            [SB],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("stub session");
    assert_eq!((stub, project.as_str(), path), (1, LONG_DIR, None));
    let s = |a: &str, k: &str| (a.to_string(), k.to_string());
    assert_eq!(
        agents_of(&conn, SB),
        vec![
            s("orchestrator", "orchestrator"),
            s("a2000000000000001", "subagent")
        ]
    );
    assert!(
        edges(&conn)
            .iter()
            .all(|(_, _, child, _)| child != "a2000000000000001"),
        "its dispatch tool_use is gone, so no edge"
    );

    // The session file comes back (a restore): the stub flag clears.
    let restored = tmp.0.join("restore");
    copy_dir(&fixture_root(), &restored);
    fs::copy(
        restored.join(format!("projects/{LONG_DIR}/{SB}.jsonl")),
        root.join(format!("projects/{LONG_DIR}/{SB}.jsonl")),
    )
    .expect("restore B");
    ingest_root(&mut store, &root);
    let stub: i64 = conn
        .query_row(
            "SELECT stub FROM sessions WHERE session_id = ?1",
            [SB],
            |r| r.get(0),
        )
        .expect("B");
    assert_eq!(stub, 0);
}

// ---- gear ---------------------------------------------------------------

fn gear_store(tmp: &Temp) -> Store {
    let root = tmp.fixture();
    let mut store = Store::open(tmp.db()).expect("open");
    store
        .ingest_graph(&discover(&root).expect("discover"))
        .expect("graph");
    store
}

#[test]
fn gear_defaults_to_80_and_95_and_agents_inherit_the_orchestrators() {
    let tmp = Temp::new("gear-inherit");
    let mut store = gear_store(&tmp);
    let a1 = Some("a1000000000000001");

    let fresh = store.gear(SA, a1).expect("gear");
    assert_eq!((fresh.nudge_pct, fresh.hard_stop_pct), (Some(80), Some(95)));
    assert_eq!(fresh.cost_cap_usd, None);

    let defaults = Gear {
        nudge_pct: Some(70),
        hard_stop_pct: Some(90),
        cost_warn_pct: Some(75),
        cost_cap_usd: Some(20.0),
        time_cap_ms: Some(3_600_000),
        cwd: Some("/work/demo-repo".into()),
        model: Some("claude-opus-5-5".into()),
        effort: Some("high".into()),
    };
    store.set_gear(SA, None, &defaults).expect("defaults");
    assert_eq!(store.gear(SA, None).expect("orchestrator"), defaults);
    assert_eq!(store.gear(SA, a1).expect("inherits"), defaults);

    let over = Gear {
        hard_stop_pct: Some(85),
        cwd: Some("/work/.worktrees/demo-repo-12".into()),
        model: Some("claude-sonnet-5".into()),
        ..Gear::default()
    };
    store.set_gear(SA, a1, &over).expect("override");
    assert_eq!(
        store.gear(SA, a1).expect("overridden"),
        Gear {
            hard_stop_pct: Some(85),
            cwd: over.cwd.clone(),
            model: over.model.clone(),
            ..defaults.clone()
        }
    );
    assert_eq!(
        store.gear(SA, Some("a1000000000000002")).expect("sibling"),
        defaults,
        "an override touches only its agent"
    );
    assert_eq!(
        store.gear(SB, None).expect("other orchestrator").nudge_pct,
        Some(80)
    );
}

#[test]
fn hard_stop_must_stay_above_nudge() {
    let tmp = Temp::new("gear-order");
    let mut store = gear_store(&tmp);
    let a1 = Some("a1000000000000001");
    let pct = |nudge, hard_stop| Gear {
        nudge_pct: nudge,
        hard_stop_pct: hard_stop,
        ..Gear::default()
    };

    assert!(store.set_gear(SA, None, &pct(Some(90), Some(90))).is_err());
    assert!(store.set_gear(SA, None, &pct(Some(91), Some(85))).is_err());
    // Against the inherited 95: a nudge of 96 inverts the pair.
    assert!(store.set_gear(SA, a1, &pct(Some(96), None)).is_err());
    assert!(
        store.set_gear(SA, None, &pct(Some(10), Some(101))).is_err(),
        "pct is 0..=100"
    );

    store
        .set_gear(SA, a1, &pct(Some(90), None))
        .expect("90 < 95");
    // Lowering the default hard stop to 85 would invert a1's effective pair.
    assert!(store.set_gear(SA, None, &pct(None, Some(85))).is_err());
    assert_eq!(store.gear(SA, a1).expect("unchanged").nudge_pct, Some(90));
    assert_eq!(
        store.gear(SA, None).expect("unchanged").hard_stop_pct,
        Some(95)
    );

    assert!(
        store
            .set_gear(SA, Some("a-not-stored"), &Gear::default())
            .is_err(),
        "gear needs a stored agent"
    );
}
