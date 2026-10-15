//! What an Audit Session row's elapsed time covers (#67): only its own
//! windows in the date filter, so a `/clear` chain's row matches the live
//! header (#65) and no stretch of wall time sits in two rows' elapsed; a
//! row nested under a director's still counts in the spans above it.
//! Every db lives in a temp dir.

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::store::views::{self, AuditFilter, AuditNode, GroupBy, NodeKind, audit};

mod common;

use common::{Temp, batch, fixture_home, transcripts};

const S: i64 = 1000;
const MIN: i64 = 60 * S;

/// A cold build of `tests/fixtures/chain-home` in UTC, and a reader on it.
fn chain_home(t: &Temp) -> (Store, Connection) {
    let home = fixture_home("chain-home");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&home).expect("discover"))
        .expect("graph");
    for path in transcripts(&home) {
        store.ingest(&batch(&path)).expect("ingest");
    }
    (store, Store::open_reader(t.db()).expect("reader"))
}

fn tree(db: &Connection, from: &str, to: &str) -> AuditNode {
    let filter = AuditFilter {
        from: from.into(),
        to: to.into(),
        repos: vec![],
        group_by: GroupBy::Hierarchy,
    };
    audit(db, &filter).expect("audit").root
}

/// Every Session row under `node`, nested ones too, by label.
fn rows<'a>(node: &'a AuditNode, out: &mut Vec<&'a AuditNode>) {
    for c in &node.children {
        if c.kind == NodeKind::Session {
            out.push(c);
        }
        rows(c, out);
    }
}

/// The one Session row anywhere in `root` labelled `label`.
fn row<'a>(root: &'a AuditNode, label: &str) -> &'a AuditNode {
    let mut all = Vec::new();
    rows(root, &mut all);
    let found: Vec<_> = all.into_iter().filter(|r| r.label == label).collect();
    assert_eq!(found.len(), 1, "Session rows labelled {label:?}");
    found[0]
}

/// The orchestrator under Session row `row`.
fn orchestrator(row: &AuditNode) -> &AuditNode {
    row.children
        .iter()
        .find(|c| c.kind == NodeKind::Agent && c.key.is_empty())
        .expect("orchestrator")
}

/// The Repo child of `node` keyed `slug`.
fn repo<'a>(node: &'a AuditNode, slug: &str) -> &'a AuditNode {
    node.children
        .iter()
        .find(|c| c.kind == NodeKind::Repo && c.key == slug)
        .unwrap_or_else(|| panic!("no repo {slug} under {:?}", node.label))
}

/// The live header's run time on workstream `name`'s tab.
fn header_run(db: &Connection, name: &str) -> Option<i64> {
    let tabs = views::workstreams(db).expect("workstreams");
    let tab = tabs.iter().find(|w| w.name == name).expect("tab");
    views::live_graph(db, tab.id)
        .expect("query")
        .expect("a graph")
        .run_ms
}

#[test]
fn a_chain_rows_elapsed_is_its_own_links_as_the_header_sums_them() {
    let t = Temp::new("chain-rows-own-links");
    let (_store, db) = chain_home(&t);
    let root = tree(&db, "2026-10-08", "2026-10-08");
    // P1 and P3, 55 s each; P2's hour in chain-repo-6 between them is that
    // row's, not this one's.
    let five = row(&root, "chain-repo-5");
    assert_eq!(five.metrics.elapsed_ms, Some(2 * 55 * S));
    assert_eq!(row(&root, "chain-repo-6").metrics.elapsed_ms, Some(55 * S));
    // L1 and L3 55 s, L2 121 s, without the hours between the links.
    let director = row(&root, "director-chain");
    assert_eq!(director.metrics.elapsed_ms, Some((55 + 121 + 55) * S));
    for name in [
        "director-chain",
        "chain-repo-5",
        "chain-repo-6",
        "ticket-chain-repo-7",
    ] {
        let r = row(&root, name);
        assert_eq!(r.metrics.elapsed_ms, header_run(&db, name), "{name}");
        assert_eq!(
            orchestrator(r).metrics.elapsed_ms,
            r.metrics.elapsed_ms,
            "{name}'s orchestrator"
        );
    }
    // The issues and the repo still span first start to last end.
    let repo = repo(&root, "chain-repo");
    let issue = |n: &str| repo.children.iter().find(|c| c.key == n).expect("issue");
    assert_eq!(issue("5").metrics.elapsed_ms, Some(7255 * S));
}

/// A store holding exactly `sql`'s rows (no transcripts), and a reader.
fn synthetic(t: &Temp, sql: &str) -> Connection {
    drop(Store::open_in(t.db(), "UTC").expect("open"));
    Connection::open(t.db())
        .expect("raw")
        .execute_batch(sql)
        .expect("rows");
    Store::open_reader(t.db()).expect("reader")
}

/// director-top's d1 (Oct 1 10:00 to 10:30) messages unit u (ticket-x-3,
/// Oct 2 11:00 to 11:20); d1's /clear successor d2 (Oct 3 09:00 to 09:10)
/// is director-top again. Each session has one $1 orchestrator cell on
/// its own day.
const SENT_EARLIER: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'director-top', 0),
        (2, 'ticket-x-3', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('d1', '/p/d1.jsonl', '2026-10-01T10:00:00.000Z', 'd1', 1),
        ('d2', '/p/d2.jsonl', '2026-10-03T09:00:00.000Z', 'd1', 1),
        ('u', '/p/u.jsonl', '2026-10-02T11:00:00.000Z', 'u', 2);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'd1', 0, '2026-10-01T10:00:00.000Z', 1), (2, 'u', 0, '2026-10-02T11:00:00.000Z', 2),
        (3, 'd2', 0, '2026-10-03T09:00:00.000Z', 1);
    INSERT INTO events (session_id, file, byte_offset, raw, sent_msg_id, timestamp, type)
    VALUES
        ('d1', '/p/d1.jsonl', 10, '{}', 'm1', '2026-10-01T10:30:00.000Z', 'assistant'),
        ('u', '/p/u.jsonl', 10, '{}', NULL, '2026-10-02T11:20:00.000Z', 'assistant'),
        ('d2', '/p/d2.jsonl', 10, '{}', NULL, '2026-10-03T09:10:00.000Z', 'assistant');
    INSERT INTO directs (child, parent, msg_id) VALUES ('u', 'd1', 'm1');
    INSERT INTO audit_cells (session_id, agent_id, segment_id, day, model, wall_ms,
        active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens) VALUES
        ('d1', NULL, 1, '2026-10-01', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('u', NULL, 2, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('d2', NULL, 3, '2026-10-03', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0);
";

/// Epoch ms of `at` on October `day`, 2026, UTC.
fn oct(day: i64, at: &str) -> i64 {
    let at: jiff::Timestamp = format!("2026-10-{day:02}T{at}Z").parse().expect("time");
    at.as_millisecond()
}

#[test]
fn a_directors_row_takes_no_span_from_a_window_outside_the_range() {
    let t = Temp::new("chain-rows-sent-earlier");
    let db = synthetic(&t, SENT_EARLIER);
    let elapsed = |from: &str, to: &str| {
        let root = tree(&db, from, to);
        let x = repo(&root, "x");
        (
            row(x, "director-top").metrics.elapsed_ms,
            row(x, "ticket-x-3").metrics.elapsed_ms,
            x.metrics.elapsed_ms,
        )
    };
    // Oct 2 holds only the unit: its director's row reads no time of its
    // own, and the repo spans the unit nested under it.
    assert_eq!(
        elapsed("2026-10-02", "2026-10-02"),
        (None, Some(20 * MIN), Some(20 * MIN))
    );
    // Oct 2 to 3: d2's window, not d1's, which sent the message on Oct 1.
    assert_eq!(
        elapsed("2026-10-02", "2026-10-03"),
        (
            Some(10 * MIN),
            Some(20 * MIN),
            Some(oct(3, "09:10:00") - oct(2, "11:00:00"))
        )
    );
    // The whole range: d1's half hour and d2's ten minutes.
    assert_eq!(
        elapsed("2026-10-01", "2026-10-03"),
        (
            Some(40 * MIN),
            Some(20 * MIN),
            Some(oct(3, "09:10:00") - oct(1, "10:00:00"))
        )
    );
}
