//! Performance inputs (#51) over the synthetic `tests/fixtures/perf-home`:
//! each dispatch's outcome from its task notifications and final report,
//! follow-ups by SendMessage, context peaks, and the API-error and
//! SendMessage columns of `events`. Every db lives in a temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use strate_core::cost::context_pct;
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

const SID: &str = "5e55e1e1-0000-4000-8000-0000000000e1";
const B: [&str; 6] = [
    "a6000000000000001",
    "a6000000000000002",
    "a6000000000000003",
    "a6000000000000004",
    "a6000000000000005",
    "a6000000000000006",
];

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-perf-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn db(&self) -> PathBuf {
        self.0.join("strate.db")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/perf-home")
}

/// Every transcript under `root/projects`, sorted: the orchestrator first.
fn transcripts() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("readable dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root().join("projects"), &mut out);
    out.sort();
    out
}

/// The whole of `path` as one batch, each line a record at its byte offset.
fn batch(path: &Path) -> Batch {
    let text = fs::read_to_string(path).expect("read");
    let path: Arc<Path> = path.into();
    let mut offset = 0u64;
    let mut events = Vec::new();
    for line in text.split_inclusive('\n') {
        events.push(TailEvent::Record {
            path: path.clone(),
            offset,
            value: serde_json::from_str(line).expect("fixture line is json"),
        });
        offset += line.len() as u64;
    }
    Batch {
        path,
        events,
        checkpoint: Some(Checkpoint {
            offset,
            identity: FileIdentity {
                volume: 0,
                index: 0,
            },
        }),
    }
}

/// The discovery graph, then each transcript as one batch in `order`.
fn build(t: &Temp, order: &[PathBuf]) -> Connection {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    for path in order {
        store.ingest(&batch(path)).expect("ingest");
    }
    drop(store);
    Connection::open(t.db()).expect("db")
}

/// An agent's (outcome, followups, context_peak_pct); `None` is the
/// orchestrator.
type Perf = (Option<String>, Option<i64>, Option<f64>);

fn perf(db: &Connection, agent: Option<&str>) -> Perf {
    db.query_row(
        "SELECT outcome, followups, context_peak_pct FROM agents
         WHERE session_id = ?1 AND agent_id IS ?2",
        params![SID, agent],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .expect("agent")
}

fn outcomes(db: &Connection) -> Vec<Option<String>> {
    B.iter().map(|b| perf(db, Some(b)).0).collect()
}

fn event_column(db: &Connection, column: &str, uuid: &str) -> Option<String> {
    db.query_row(
        &format!("SELECT {column} FROM events WHERE uuid = ?1"),
        [uuid],
        |r| r.get(0),
    )
    .optional()
    .expect(column)
    .flatten()
}

#[test]
fn an_outcome_is_the_worst_notified_status_and_a_needs_context_report_is_blocked() {
    let t = Temp::new("outcomes");
    let db = build(&t, &transcripts());
    let s = |v: &str| Some(v.to_string());
    // b1 completed four times over; b2 completed then failed; b3 killed; b4
    // completed with a NEEDS_CONTEXT: line in its handback; b5 completed,
    // quoting NEEDS_CONTEXT mid-line in its last text; b6 never notified.
    assert_eq!(
        outcomes(&db),
        vec![
            s("completed"),
            s("failed"),
            s("killed"),
            s("blocked"),
            s("completed"),
            None
        ]
    );
    assert_eq!(perf(&db, None).0, None, "the orchestrator has no outcome");
}

#[test]
fn notifications_are_read_from_their_carriers_only() {
    let t = Temp::new("carriers");
    let db = build(&t, &transcripts());
    let rows = |task: &str| -> Vec<(Option<String>, Option<String>)> {
        db.prepare(
            "SELECT type, notification_status FROM events
             WHERE notification_task = ?1 ORDER BY timestamp, byte_offset",
        )
        .expect("rows")
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    };
    let row = |kind: &str, status: &str| (Some(kind.to_string()), Some(status.to_string()));
    // Two queue operations, the queued_command attachment and the prompt.
    assert_eq!(
        rows(B[0]),
        vec![
            row("queue-operation", "completed"),
            row("queue-operation", "completed"),
            row("attachment", "completed"),
            row("user", "completed"),
        ]
    );
    // A typed-looking prompt with no origin is a notification by its text.
    assert_eq!(rows(B[2]), vec![row("user", "killed")]);
    // Quoted in a Bash command and its output: not a notification.
    assert_eq!(rows(B[5]), vec![]);
    // A background shell's is stored, and matches no agent.
    assert_eq!(rows("bshell001"), vec![row("queue-operation", "failed")]);
}

#[test]
fn followups_count_send_messages_by_agent_id_or_name() {
    let t = Temp::new("followups");
    let db = build(&t, &transcripts());
    // b1 by id from the orchestrator; b2 (perf-helper) by name from the
    // orchestrator and from b1; "nobody" matches no agent.
    let followups: Vec<Option<i64>> = B.iter().map(|b| perf(&db, Some(b)).1).collect();
    assert_eq!(
        followups,
        vec![Some(1), Some(2), Some(0), Some(0), Some(0), Some(0)]
    );
    assert_eq!(
        perf(&db, None).1,
        None,
        "the orchestrator is not followed up"
    );
    assert_eq!(
        event_column(&db, "message_to", "g-23").as_deref(),
        Some(r#"["perf-helper","nobody"]"#)
    );
    assert_eq!(
        event_column(&db, "message_to", "g-21").as_deref(),
        Some(r#"["a6000000000000001"]"#)
    );
    assert_eq!(event_column(&db, "message_to", "g-18"), None);
}

#[test]
fn the_context_peak_is_the_deepest_request_not_the_latest() {
    let t = Temp::new("peak");
    let db = build(&t, &transcripts());
    // b1: input 1000 + cache read 99000 = 100k, then 500 + 9500 = 10k.
    let sonnet = |depth| context_pct(depth, "claude-sonnet-5-5").expect("window");
    let (_, _, peak) = perf(&db, Some(B[0]));
    assert_eq!(peak, Some(sonnet(100_000)));
    let latest: f64 = db
        .query_row(
            "SELECT context_pct FROM agents WHERE agent_id = ?1",
            [B[0]],
            |r| r.get(0),
        )
        .expect("latest");
    assert_eq!(latest, sonnet(10_000));
    // Every orchestrator request is 1000 input + 10000 cache read.
    let opus = context_pct(11_000, "claude-opus-5-5").expect("window");
    assert_eq!(perf(&db, None).2, Some(opus));
}

#[test]
fn api_errors_split_into_prompt_too_long_and_other() {
    let t = Temp::new("api-errors");
    let db = build(&t, &transcripts());
    assert_eq!(
        event_column(&db, "api_error", "g-24").as_deref(),
        Some("prompt_too_long")
    );
    assert_eq!(
        event_column(&db, "api_error", "g-25").as_deref(),
        Some("other")
    );
    let flagged: i64 = db
        .query_row(
            "SELECT count(*) FROM events WHERE api_error IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(flagged, 2);
}

#[test]
fn any_ingest_order_converges() {
    let t = Temp::new("order-fwd");
    let forward = build(&t, &transcripts());
    let r = Temp::new("order-rev");
    let mut reversed: Vec<PathBuf> = transcripts();
    reversed.reverse();
    let backward = build(&r, &reversed);
    let all = |db: &Connection| -> Vec<Perf> {
        std::iter::once(None)
            .chain(B.iter().map(|b| Some(*b)))
            .map(|a| perf(db, a))
            .collect()
    };
    assert_eq!(all(&forward), all(&backward));
}

#[test]
fn opening_a_store_from_an_older_rollup_version_backfills_every_column() {
    let t = Temp::new("backfill");
    let db = build(&t, &transcripts());
    let want = outcomes(&db);
    db.execute_batch(
        "UPDATE events SET notification_task = NULL, notification_status = NULL,
             message_to = NULL, api_error = NULL;
         UPDATE agents SET outcome = NULL, followups = NULL, context_peak_pct = NULL;
         UPDATE meta SET value = '2' WHERE key = 'rollup';",
    )
    .expect("an older store");
    drop(db);
    drop(Store::open_in(t.db(), "UTC").expect("reopen"));
    let db = Connection::open(t.db()).expect("db");
    assert_eq!(outcomes(&db), want);
    assert_eq!(perf(&db, Some(B[1])).1, Some(2));
    assert_eq!(
        event_column(&db, "api_error", "g-24").as_deref(),
        Some("prompt_too_long")
    );
}
