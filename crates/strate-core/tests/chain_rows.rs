//! What an Audit Session row's elapsed time covers (#67, #107): a Session
//! row is a `/clear` chain's sessions within one charge, and its elapsed is
//! only those sessions' time in the date filter, so no stretch of wall time
//! sits in two rows' elapsed. A session another one messaged counts under
//! its own charge, not nested under its messager's row. Every db lives in a
//! temp dir.

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::store::views::{AuditFilter, AuditNode, NodeKind, audit};

mod common;

use common::{Temp, batch, fixture_home, transcripts};

const S: i64 = 1000;
const MIN: i64 = 60 * S;

/// A cold build of `tests/fixtures/chain-home` in UTC, and a reader on it.
fn chain_home(t: &Temp) -> Connection {
    let home = fixture_home("chain-home");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&home).expect("discover"))
        .expect("graph");
    for path in transcripts(&home) {
        store.ingest(&batch(&path)).expect("ingest");
    }
    drop(store);
    Store::open_reader(t.db()).expect("reader")
}

fn tree(db: &Connection, from: &str, to: &str) -> AuditNode {
    let filter = AuditFilter {
        from: from.into(),
        to: to.into(),
        repos: vec![],
    };
    audit(db, &filter).expect("audit").root
}

/// Every Session row under `node`, by label.
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

/// The child of `node` of `kind` keyed `key`.
fn child<'a>(node: &'a AuditNode, kind: NodeKind, key: &str) -> &'a AuditNode {
    node.children
        .iter()
        .find(|c| c.kind == kind && c.key == key)
        .unwrap_or_else(|| panic!("no {kind:?} {key:?} under {:?}", node.label))
}

/// The labels of the Session rows directly under `node`.
fn session_labels(node: &AuditNode) -> Vec<&str> {
    node.children
        .iter()
        .filter(|c| c.kind == NodeKind::Session)
        .map(|c| c.label.as_str())
        .collect()
}

#[test]
fn a_chain_rows_elapsed_is_its_own_links_in_its_charge() {
    let t = Temp::new("chain-rows-own-links");
    let db = chain_home(&t);
    let root = tree(&db, "2026-10-08", "2026-10-08");
    // P1 and P3 (chain-repo-5), 55 s each; P2's hour in chain-repo-6
    // between them is that row's, not this one's.
    let five = row(&root, "chain-repo-5");
    assert_eq!(five.metrics.elapsed_ms, Some(2 * 55 * S));
    assert_eq!(five.metrics.sessions, 2);
    assert_eq!(row(&root, "chain-repo-6").metrics.elapsed_ms, Some(55 * S));
    // L1 and L3 55 s, L2 121 s (its message at 11:02:01), without the
    // hours between the links.
    let director = row(&root, "director-chain");
    assert_eq!(director.metrics.elapsed_ms, Some((55 + 121 + 55) * S));
    // U1 from the message it received at 11:02:02 to 11:03:55 (113 s), U2
    // 55 s.
    let unit = row(&root, "ticket-chain-repo-7");
    assert_eq!(unit.metrics.elapsed_ms, Some((113 + 55) * S));
    for r in [five, director, unit] {
        assert_eq!(
            orchestrator(r).metrics.elapsed_ms,
            r.metrics.elapsed_ms,
            "{}'s orchestrator",
            r.label
        );
    }
    // The director is a topic; its unit sits under its own issue, not
    // nested under the director's row.
    let topic = child(&root, NodeKind::Topic, "chain");
    assert_eq!(session_labels(topic), ["director-chain"]);
    let repo = child(&root, NodeKind::Repo, "chain-repo");
    let seven = child(repo, NodeKind::Issue, "7");
    assert_eq!(session_labels(seven), ["ticket-chain-repo-7"]);
    // The issues still span first start to last end.
    assert_eq!(
        child(repo, NodeKind::Issue, "5").metrics.elapsed_ms,
        Some(7255 * S)
    );
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
/// Oct 2 11:00 to 11:20) and the unnamed w (Oct 2 12:00 to 12:05); d1's
/// /clear successor d2 (Oct 3 09:00 to 09:10) is director-top again. Each
/// session has one $1 orchestrator cell on its own day.
const MESSAGED: &str = "
    INSERT INTO sessions (session_id, path, started_at, process_root) VALUES
        ('d1', '/p/d1.jsonl', '2026-10-01T10:00:00.000Z', 'd1'),
        ('d2', '/p/d2.jsonl', '2026-10-03T09:00:00.000Z', 'd1'),
        ('u', '/p/u.jsonl', '2026-10-02T11:00:00.000Z', 'u'),
        ('w', '/p/w.jsonl', '2026-10-02T12:00:00.000Z', 'w');
    INSERT INTO charges (id, key, kind, repo, number, topic) VALUES
        (1, 'topic_top', 'topic', NULL, NULL, 'top'),
        (2, 'issue_x-3', 'issue', 'x', 3, NULL);
    INSERT INTO agents (id, session_id, kind, name, own_charge, own_charge_source) VALUES
        (1, 'd1', 'orchestrator', 'director-top', 1, 'name'),
        (2, 'd2', 'orchestrator', 'director-top', 1, 'name'),
        (3, 'u', 'orchestrator', 'ticket-x-3', 2, 'name'),
        (4, 'w', 'orchestrator', NULL, NULL, NULL);
    INSERT INTO edges (kind, parent, child) VALUES ('message', 1, 3), ('message', 1, 4);
    INSERT INTO events (session_id, file, byte_offset, raw, timestamp, type) VALUES
        ('d1', '/p/d1.jsonl', 10, '{}', '2026-10-01T10:30:00.000Z', 'assistant'),
        ('u', '/p/u.jsonl', 10, '{}', '2026-10-02T11:20:00.000Z', 'assistant'),
        ('w', '/p/w.jsonl', 10, '{}', '2026-10-02T12:05:00.000Z', 'assistant'),
        ('d2', '/p/d2.jsonl', 10, '{}', '2026-10-03T09:10:00.000Z', 'assistant');
    INSERT INTO audit_cells (session_id, agent_id, day, model, wall_ms, active_ms, wait_ms,
        cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
        cache_write_tokens) VALUES
        ('d1', NULL, '2026-10-01', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('u', NULL, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('w', NULL, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('d2', NULL, '2026-10-03', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0);
";

#[test]
fn a_messaged_session_counts_under_its_own_charge_and_a_chain_spans_its_links_in_range() {
    let t = Temp::new("chain-rows-messaged");
    let db = synthetic(&t, MESSAGED);
    // Oct 2: u under its own issue, $1; w, with no charge of its own, under
    // its messager's topic as a row of its own. No director row: d1 has no
    // cell that day, and nothing nests under it.
    let oct2 = tree(&db, "2026-10-02", "2026-10-02");
    let kinds: Vec<_> = oct2
        .children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect();
    assert_eq!(kinds, [(NodeKind::Repo, "x"), (NodeKind::Topic, "top")]);
    let three = child(child(&oct2, NodeKind::Repo, "x"), NodeKind::Issue, "3");
    assert_eq!(session_labels(three), ["ticket-x-3"]);
    assert_eq!(row(&oct2, "ticket-x-3").metrics.elapsed_ms, Some(20 * MIN));
    assert_eq!(three.metrics.cost_usd, 1.0);
    let top = child(&oct2, NodeKind::Topic, "top");
    assert_eq!(top.label, "top");
    assert_eq!(session_labels(top), ["w"]);
    assert_eq!(row(&oct2, "w").metrics.elapsed_ms, Some(5 * MIN));

    // Oct 2 to 3: the director's row holds d2's ten minutes, not d1's.
    let elapsed =
        |from: &str, to: &str| row(&tree(&db, from, to), "director-top").metrics.elapsed_ms;
    assert_eq!(elapsed("2026-10-02", "2026-10-03"), Some(10 * MIN));
    // The whole range: d1's half hour and d2's ten minutes, one row.
    assert_eq!(elapsed("2026-10-01", "2026-10-03"), Some(40 * MIN));
    let all = tree(&db, "2026-10-01", "2026-10-03");
    let top = child(&all, NodeKind::Topic, "top");
    assert_eq!(session_labels(top), ["director-top", "w"]);
    assert_eq!(row(&all, "director-top").metrics.sessions, 2);
}
