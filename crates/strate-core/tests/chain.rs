//! A `/clear` chain read as one session (#63), over the synthetic
//! `tests/fixtures/chain-home`: the Audit's one Session row per chain and
//! workstream, a director's `--bg` unit nested under the director's row,
//! and the live graph drawing every link's subagents, earlier links' marked.
//! Every db lives in a temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{
    self, AuditFilter, AuditNode, GraphAgent, GraphEdge, GroupBy, LiveGraph, Metrics, NodeKind,
    SpanKind, Workstream, audit, audit_timeline,
};
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

/// director-chain's third and last link.
const L3: &str = "5e55c4c4-0000-4000-8000-0000000000a3";
/// ticket-chain-repo-7's second link, after /clear: never messaged itself.
const U2: &str = "5e55c4c4-0000-4000-8000-0000000000b2";
/// chain-repo-6, between two chain-repo-5 links of one process.
const P2: &str = "5e55c4c4-0000-4000-8000-0000000000c2";
/// chain-repo-5's second link.
const P3: &str = "5e55c4c4-0000-4000-8000-0000000000c3";
/// The subagents of L1, L2, L3, U1, P1, P2, P3 and U2.
const J: [&str; 8] = [
    "a8000000000000001",
    "a8000000000000002",
    "a8000000000000003",
    "a8000000000000004",
    "a8000000000000005",
    "a8000000000000006",
    "a8000000000000007",
    "a8000000000000008",
];
const S: i64 = 1000;
/// Every orchestrator request is opus (1000 in, 100 out, 10000 read):
/// $0.008; every subagent's sonnet (500 in, 200 out, 5000 read): $0.0035.
const OPUS: f64 = 0.008;
const SONNET: f64 = 0.0035;

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-chain-{name}-{}", std::process::id()));
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

fn home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chain-home")
}

/// Every transcript under `home/projects`, sorted.
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
    walk(&home().join("projects"), &mut out);
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

/// A cold build in UTC, and a reader on it.
fn build(t: &Temp) -> (Store, Connection) {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&home()).expect("discover"))
        .expect("graph");
    for path in transcripts() {
        store.ingest(&batch(&path)).expect("ingest");
    }
    let reader = Store::open_reader(t.db()).expect("reader");
    (store, reader)
}

fn oct8(group_by: GroupBy) -> AuditFilter {
    AuditFilter {
        from: "2026-10-08".into(),
        to: "2026-10-08".into(),
        repos: vec![],
        group_by,
    }
}

fn tree(db: &Connection) -> AuditNode {
    audit(db, &oct8(GroupBy::Hierarchy)).expect("audit").root
}

/// The children of `node` of `kind`, by label.
fn labels(node: &AuditNode, kind: NodeKind) -> Vec<&str> {
    node.children
        .iter()
        .filter(|c| c.kind == kind)
        .map(|c| c.label.as_str())
        .collect()
}

/// The one child of `node` of `kind` labelled `label`.
fn labelled<'a>(node: &'a AuditNode, kind: NodeKind, label: &str) -> &'a AuditNode {
    let found: Vec<_> = node
        .children
        .iter()
        .filter(|c| c.kind == kind && c.label == label)
        .collect();
    assert_eq!(found.len(), 1, "{kind:?} {label:?} under {:?}", node.label);
    found[0]
}

/// The Agent children of a Session row by key (`""`: the orchestrator).
fn agents(row: &AuditNode) -> Vec<&str> {
    let mut keys: Vec<&str> = row
        .children
        .iter()
        .filter(|c| c.kind == NodeKind::Agent)
        .map(|c| c.key.as_str())
        .collect();
    keys.sort();
    keys
}

fn close(want: f64, got: f64) -> bool {
    (want - got).abs() < 1e-9
}

#[test]
fn a_clear_chain_is_one_session_row_holding_every_links_subagents() {
    let t = Temp::new("one-row");
    let (_store, db) = build(&t);
    let root = tree(&db);
    let unlinked = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Unlinked)
        .expect("unlinked");
    assert_eq!(labels(unlinked, NodeKind::Session), ["director-chain"]);
    let row = labelled(unlinked, NodeKind::Session, "director-chain");
    // One orchestrator node for the three links, and each link's subagent.
    assert_eq!(agents(row), ["", J[0], J[1], J[2]]);
    let orchestrator = row.children.iter().find(|c| c.key.is_empty()).unwrap();
    // L1 10:00:00 to L3's last record 12:00:55: the chain's span.
    assert_eq!(row.metrics.elapsed_ms, Some(7255 * S));
    assert_eq!(orchestrator.metrics.elapsed_ms, Some(7255 * S));
}

#[test]
fn a_directed_unit_and_its_clear_successor_nest_under_the_directors_row() {
    let t = Temp::new("unit");
    let (_store, db) = build(&t);
    let root = tree(&db);
    let unlinked = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Unlinked)
        .expect("unlinked");
    let director = labelled(unlinked, NodeKind::Session, "director-chain");
    assert_eq!(labels(director, NodeKind::Session), ["ticket-chain-repo-7"]);
    let unit = labelled(director, NodeKind::Session, "ticket-chain-repo-7");
    // U1 (messaged) and U2 (its /clear successor): one row, both subagents.
    assert_eq!(agents(unit), ["", J[3], J[7]]);
    // U1's first record 11:02:02 to U2's last 11:30:55.
    assert_eq!(unit.metrics.elapsed_ms, Some(1733 * S));
    let m = &unit.metrics;
    assert_eq!((m.sessions, m.agents, m.rotations), (2, 2, 0));
    assert!(
        close(4.0 * OPUS + 2.0 * SONNET, m.cost_usd),
        "{}",
        m.cost_usd
    );
    // The director's row holds its own 3 windows and the unit's 2.
    let m = &director.metrics;
    assert_eq!((m.sessions, m.agents), (5, 5));
    // 7 director requests (2 a link, plus L2's SendMessage), 4 unit ones.
    assert!(
        close(11.0 * OPUS + 5.0 * SONNET, m.cost_usd),
        "{}",
        m.cost_usd
    );
    // The unit's work left its own repo and issue: only the chain-repo-5
    // and -6 rows sit under the repo.
    let repo = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Repo && c.key == "chain-repo")
        .expect("repo");
    let mut issues: Vec<&str> = repo.children.iter().map(|c| c.key.as_str()).collect();
    issues.sort();
    assert_eq!(issues, ["5", "6"]);
}

#[test]
fn a_chain_reads_per_workstream_where_each_name_puts_it() {
    let t = Temp::new("per-workstream");
    let (_store, db) = build(&t);
    let root = tree(&db);
    let repo = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Repo && c.key == "chain-repo")
        .expect("repo");
    let issue = |n: &str| repo.children.iter().find(|c| c.key == n).expect("issue");
    // P1 and P3 share chain-repo-5: one row; P2's chain-repo-6 is its own.
    let five = labelled(issue("5"), NodeKind::Session, "chain-repo-5");
    assert_eq!(labels(issue("5"), NodeKind::Session), ["chain-repo-5"]);
    assert_eq!(agents(five), ["", J[4], J[6]]);
    assert_eq!(five.metrics.sessions, 2);
    // P1 13:00:00 to P3's last record 15:00:55.
    assert_eq!(five.metrics.elapsed_ms, Some(7255 * S));
    let six = labelled(issue("6"), NodeKind::Session, "chain-repo-6");
    assert_eq!(agents(six), ["", J[5]]);
    assert_eq!(six.metrics.sessions, 1);
}

/// The additive metrics of `m`: $, tokens, agent ms, windows, subagents.
fn additive(m: &Metrics) -> (f64, i64, i64, i64, i64) {
    let t = &m.tokens;
    (
        m.cost_usd,
        t.input + t.output + t.cache_read + t.cache_write,
        m.agent_ms,
        m.sessions,
        m.agents,
    )
}

fn check_sums(node: &AuditNode) {
    if node.children.is_empty() {
        return;
    }
    let mut sum = (0.0, 0, 0, 0, 0);
    for c in &node.children {
        let k = additive(&c.metrics);
        sum = (
            sum.0 + k.0,
            sum.1 + k.1,
            sum.2 + k.2,
            sum.3 + k.3,
            sum.4 + k.4,
        );
        check_sums(c);
    }
    let own = additive(&node.metrics);
    assert!(close(own.0, sum.0), "{} $", node.label);
    assert_eq!(
        (own.1, own.2, own.3, own.4),
        (sum.1, sum.2, sum.3, sum.4),
        "{}",
        node.label
    );
}

#[test]
fn each_cell_counts_once_whatever_the_nesting() {
    let t = Temp::new("totals");
    let (_store, db) = build(&t);
    // 8 links of 3 processes, 2 opus requests each plus L2's SendMessage;
    // 8 subagents.
    for group_by in [
        GroupBy::Hierarchy,
        GroupBy::Day,
        GroupBy::Pr,
        GroupBy::AgentType,
        GroupBy::Model,
    ] {
        let root = audit(&db, &oct8(group_by)).expect("audit").root;
        check_sums(&root);
        let m = &root.metrics;
        assert!(
            close(17.0 * OPUS + 8.0 * SONNET, m.cost_usd),
            "{group_by:?}"
        );
        assert_eq!((m.sessions, m.agents), (8, 8), "{group_by:?}");
    }
}

#[test]
fn a_nested_rows_timeline_is_its_own_cells() {
    let t = Temp::new("timeline");
    let (_store, db) = build(&t);
    let root = tree(&db);
    let unlinked = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Unlinked)
        .expect("unlinked");
    let director = labelled(unlinked, NodeKind::Session, "director-chain");
    let unit = labelled(director, NodeKind::Session, "ticket-chain-repo-7");
    let path = vec![
        (NodeKind::Unlinked, String::new()),
        (NodeKind::Session, director.key.clone()),
        (NodeKind::Session, unit.key.clone()),
    ];
    let days = audit_timeline(&db, &oct8(GroupBy::Hierarchy), &path).expect("timeline");
    assert_eq!(days.len(), 1);
    let total = |kind| -> i64 {
        days[0]
            .spans
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.end_ms - s.start_ms)
            .sum()
    };
    assert_eq!(
        total(SpanKind::Active) + total(SpanKind::Wait),
        unit.metrics.active_ms
    );
    assert_eq!(total(SpanKind::Agent), unit.metrics.agent_ms);
    // Only the unit's spans: U1 from 11:02:02, U2 to 11:30:55.
    let first = days[0].spans.iter().map(|s| s.start_ms).min().unwrap();
    let last = days[0].spans.iter().map(|s| s.end_ms).max().unwrap();
    assert_eq!(
        (first, last),
        ((11 * 3600 + 122) * S, (11 * 3600 + 1855) * S)
    );
}

/// A registry entry for `session`, interactive and busy under pid `pid`.
fn listed(pid: u32, session: &str, name: &str) -> RegistryEvent {
    let json = format!(
        r#"[{{"pid":{pid},"cwd":"/work/hub","kind":"interactive","startedAt":1791000000000,
             "sessionId":"{session}","name":"{name}","status":"busy"}}]"#
    );
    RegistryEvent::Changed {
        entry: parse_snapshot(json.as_bytes()).expect("entry").remove(0),
        at_ms: 5000,
    }
}

fn tab<'a>(rows: &'a [Workstream], name: &str) -> &'a Workstream {
    rows.iter()
        .find(|w| w.name == name)
        .unwrap_or_else(|| panic!("no workstream {name}: {rows:?}"))
}

fn graph(db: &Connection, name: &str) -> LiveGraph {
    let rows = views::workstreams(db).expect("workstreams");
    views::live_graph(db, tab(&rows, name).id)
        .expect("query")
        .expect("a graph")
}

/// Each drawn agent: its agent id (`-` for an orchestrator), name and
/// whether it is of an earlier link.
fn drawn(g: &LiveGraph) -> Vec<(&str, &str, bool)> {
    g.agents
        .iter()
        .map(|a: &GraphAgent| {
            (
                a.agent_id.as_deref().unwrap_or("-"),
                a.name.as_deref().unwrap_or("-"),
                a.earlier,
            )
        })
        .collect()
}

fn edge(kind: &str, parent: i64, child: i64) -> GraphEdge {
    GraphEdge {
        kind: kind.into(),
        parent,
        child,
    }
}

#[test]
fn the_director_draws_every_links_subagents_and_its_units_chain() {
    let t = Temp::new("graph");
    let (_store, db) = build(&t);
    let g = graph(&db, "director-chain");
    assert_eq!(g.session_id, L3);
    assert_eq!(
        drawn(&g),
        [
            ("-", "director-chain", false),
            (J[2], "-", false),
            // L1's and L2's, in link order: under the drawn orchestrator.
            (J[0], "-", true),
            (J[1], "-", true),
            // The unit draws its last link, U2, and U1's subagent.
            ("-", "ticket-chain-repo-7", false),
            (J[7], "-", false),
            (J[3], "-", true),
        ]
    );
    let id = |i: usize| g.agents[i].id;
    assert_eq!(
        g.edges,
        [
            edge("dispatch", id(0), id(1)),
            edge("dispatch", id(0), id(2)),
            edge("dispatch", id(0), id(3)),
            edge("directs", id(0), id(4)),
            edge("dispatch", id(4), id(5)),
            edge("dispatch", id(4), id(6)),
        ]
    );
    assert_eq!(
        g.agents.iter().map(|a| a.parent).collect::<Vec<_>>(),
        [
            None,
            Some(id(0)),
            Some(id(0)),
            Some(id(0)),
            Some(id(0)),
            Some(id(4)),
            Some(id(4))
        ]
    );
}

#[test]
fn a_chain_draws_only_its_links_in_the_drawn_workstream() {
    let t = Temp::new("graph-workstreams");
    let (_store, db) = build(&t);
    let five = graph(&db, "chain-repo-5");
    assert_eq!(five.session_id, P3);
    assert_eq!(
        drawn(&five),
        [
            ("-", "chain-repo-5", false),
            (J[6], "-", false),
            (J[4], "-", true),
        ]
    );
    let six = graph(&db, "chain-repo-6");
    assert_eq!(six.session_id, P2);
    assert_eq!(
        drawn(&six),
        [("-", "chain-repo-6", false), (J[5], "-", false)]
    );
}

#[test]
fn a_unit_tab_nests_whichever_link_of_its_chain_was_directed() {
    let t = Temp::new("nest");
    let (mut store, db) = build(&t);
    store
        .apply_registry(&[
            listed(4201, L3, "director-chain"),
            listed(4202, U2, "ticket-chain-repo-7"),
        ])
        .expect("live");
    let rows = views::workstreams(&db).expect("workstreams");
    let unit = tab(&rows, "ticket-chain-repo-7");
    // U2 is drawn; U1, its earlier link, was the one messaged.
    assert_eq!(unit.session_id.as_deref(), Some(U2));
    assert_eq!(unit.director, Some(tab(&rows, "director-chain").id));
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

/// director-top (d1, its only cell on Oct 1) directs director-mid (d2),
/// which directs ticket-x-3 (u). Rows alpha-1 (a1, a2: one process) and
/// beta-2 (b1, b2) direct each other: b1 launched a1, a2 launched b2. Each
/// session has one $1 orchestrator cell.
const NESTED: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'director-top', 0),
        (2, 'director-mid', 0), (3, 'ticket-x-3', 0), (4, 'alpha-1', 0), (5, 'beta-2', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('d1', '/p/d1.jsonl', '2026-10-01T10:00:00.000Z', 'd1', 1),
        ('d2', '/p/d2.jsonl', '2026-10-02T10:00:00.000Z', 'd2', 2),
        ('u', '/p/u.jsonl', '2026-10-02T11:00:00.000Z', 'u', 3),
        ('a1', '/p/a1.jsonl', '2026-10-02T12:00:00.000Z', 'pa', 4),
        ('a2', '/p/a2.jsonl', '2026-10-02T13:00:00.000Z', 'pa', 4),
        ('b1', '/p/b1.jsonl', '2026-10-02T12:30:00.000Z', 'pb', 5),
        ('b2', '/p/b2.jsonl', '2026-10-02T13:30:00.000Z', 'pb', 5);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'd1', 0, '2026-10-01T10:00:00.000Z', 1), (2, 'd2', 0, '2026-10-02T10:00:00.000Z', 2),
        (3, 'u', 0, '2026-10-02T11:00:00.000Z', 3), (4, 'a1', 0, '2026-10-02T12:00:00.000Z', 4),
        (5, 'a2', 0, '2026-10-02T13:00:00.000Z', 4), (6, 'b1', 0, '2026-10-02T12:30:00.000Z', 5),
        (7, 'b2', 0, '2026-10-02T13:30:00.000Z', 5);
    INSERT INTO events (session_id, file, byte_offset, raw, sent_msg_id) VALUES
        ('d1', '/p/d1.jsonl', 10, '{}', 'm1'), ('d2', '/p/d2.jsonl', 10, '{}', 'm2'),
        ('b1', '/p/b1.jsonl', 10, '{}', 'm3'), ('a2', '/p/a2.jsonl', 10, '{}', 'm4');
    INSERT INTO directs (child, parent, msg_id) VALUES
        ('d2', 'd1', 'm1'), ('u', 'd2', 'm2'), ('a1', 'b1', 'm3'), ('b2', 'a2', 'm4');
    INSERT INTO audit_cells (session_id, agent_id, segment_id, day, model, wall_ms,
        active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens) VALUES
        ('d1', NULL, 1, '2026-10-01', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('d2', NULL, 2, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('u', NULL, 3, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('a1', NULL, 4, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('a2', NULL, 5, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('b1', NULL, 6, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('b2', NULL, 7, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0);
";

fn oct2(db: &Connection) -> AuditNode {
    let f = AuditFilter {
        from: "2026-10-02".into(),
        to: "2026-10-02".into(),
        repos: vec![],
        group_by: GroupBy::Hierarchy,
    };
    audit(db, &f).expect("audit").root
}

#[test]
fn a_unit_of_a_unit_nests_two_deep_under_a_director_outside_the_range() {
    let t = Temp::new("two-deep");
    let db = synthetic(&t, NESTED);
    let root = oct2(&db);
    check_sums(&root);
    let unlinked = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Unlinked)
        .expect("unlinked");
    // d1 has no cell on Oct 2, but its row still reads by its name.
    assert_eq!(labels(unlinked, NodeKind::Session), ["director-top"]);
    let top = labelled(unlinked, NodeKind::Session, "director-top");
    let mid = labelled(top, NodeKind::Session, "director-mid");
    let unit = labelled(mid, NodeKind::Session, "ticket-x-3");
    assert_eq!((top.metrics.sessions, mid.metrics.sessions), (2, 2));
    assert!(close(1.0, unit.metrics.cost_usd));
    assert!(!root.children.iter().any(|c| c.key == "ticket-x"));
}

#[test]
fn rows_that_direct_each_other_nest_one_way_only() {
    let t = Temp::new("cycle");
    let db = synthetic(&t, NESTED);
    let root = oct2(&db);
    check_sums(&root);
    assert!(close(6.0, root.metrics.cost_usd));
    let repos: Vec<&str> = root
        .children
        .iter()
        .filter(|c| c.kind == NodeKind::Repo)
        .map(|c| c.key.as_str())
        .collect();
    assert_eq!(repos.len(), 1, "{repos:?}");
    let issue = &root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Repo)
        .expect("repo")
        .children[0];
    let rows = labels(issue, NodeKind::Session);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let outer = &issue.children[0];
    let inner: Vec<&AuditNode> = outer
        .children
        .iter()
        .filter(|c| c.kind == NodeKind::Session)
        .collect();
    assert_eq!(inner.len(), 1);
    assert!(inner[0].children.iter().all(|c| c.kind == NodeKind::Agent));
    assert!(close(4.0, outer.metrics.cost_usd));
}

/// One process (pa) under issue xrepo#9, which has PRs 11 and 12: s1 linked
/// no PR, s2 linked PR 12. Each has one $1 orchestrator cell on Oct 2.
const ROW_PR: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'xrepo-9', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('s1', '/p/s1.jsonl', '2026-10-02T10:00:00.000Z', 'pa', 1),
        ('s2', '/p/s2.jsonl', '2026-10-02T11:00:00.000Z', 'pa', 1);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 's1', 0, '2026-10-02T10:00:00.000Z', 1), (2, 's2', 0, '2026-10-02T11:00:00.000Z', 1);
    INSERT INTO gh_repos (slug, owner, name, fetched_at) VALUES ('xrepo', 'o', 'xrepo', 1);
    INSERT INTO gh_issues (repo, number, title, state, created_at, sub_total, sub_completed)
        VALUES ('xrepo', 9, 'T', 'OPEN', '2026-10-01T00:00:00Z', 0, 0);
    INSERT INTO gh_prs (repo, issue, number, state, created_at, merged_at, url) VALUES
        ('xrepo', 9, 11, 'MERGED', '2026-10-02T00:00:00Z', '2026-10-02T12:00:00Z',
         'https://github.com/o/xrepo/pull/11'),
        ('xrepo', 9, 12, 'OPEN', '2026-10-02T00:00:00Z', NULL,
         'https://github.com/o/xrepo/pull/12');
    INSERT INTO github_links (session_id, pr_number, pr_url, pr_repository) VALUES
        ('s2', 12, 'https://github.com/o/xrepo/pull/12', 'o/xrepo');
    INSERT INTO audit_cells (session_id, agent_id, segment_id, day, model, wall_ms,
        active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens) VALUES
        ('s1', NULL, 1, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('s2', NULL, 2, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0);
";

#[test]
fn a_chains_row_reads_under_the_pr_any_of_its_links_names() {
    let t = Temp::new("row-pr");
    let db = synthetic(&t, ROW_PR);
    let root = oct2(&db);
    check_sums(&root);
    let issue = root
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Repo && c.key == "xrepo")
        .expect("repo")
        .children
        .iter()
        .find(|c| c.kind == NodeKind::Issue && c.key == "9")
        .expect("issue");
    // One place for the whole row: PR 12, which s2 linked. s1 is not left
    // directly under the issue as a second row with the same key.
    let under: Vec<(NodeKind, &str)> = issue
        .children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect();
    assert_eq!(under, [(NodeKind::Pr, "12")]);
    let row = labelled(&issue.children[0], NodeKind::Session, "xrepo-9");
    assert_eq!(row.key, "pa/1");
    assert_eq!(row.metrics.sessions, 2);
}

/// Director d (workstream 1) directed y, u1 and u2, one process (pu). y is
/// in ticket-x-1; u1 was renamed from ticket-x-1 (where it dispatched k1)
/// into ticket-x-2, and u2 is back in ticket-x-1. So the unit chain reads
/// in both workstreams, and u1 is both ticket-x-2's latest link and an
/// earlier link in ticket-x-1. y, directed first, puts ticket-x-1's unit
/// (u2) first.
const TWO_WORKSTREAM_UNIT: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'director-d', 0),
        (2, 'ticket-x-1', 0), (3, 'ticket-x-2', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('d', '/p/d.jsonl', '2026-10-02T09:00:00.000Z', 'd', 1),
        ('y', '/p/y.jsonl', '2026-10-02T09:30:00.000Z', 'pu', 2),
        ('u1', '/p/u1.jsonl', '2026-10-02T10:00:00.000Z', 'pu', 3),
        ('u2', '/p/u2.jsonl', '2026-10-02T11:00:00.000Z', 'pu', 2);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'd', 0, '2026-10-02T09:00:00.000Z', 1),
        (2, 'u1', 0, '2026-10-02T10:00:00.000Z', 2), (3, 'u1', 100, '2026-10-02T10:30:00.000Z', 3),
        (4, 'u2', 0, '2026-10-02T11:00:00.000Z', 2), (5, 'y', 0, '2026-10-02T09:30:00.000Z', 2);
    INSERT INTO agents (id, session_id, agent_id, kind, segment_id) VALUES
        (1, 'd', NULL, 'orchestrator', NULL), (2, 'u1', NULL, 'orchestrator', NULL),
        (3, 'u2', NULL, 'orchestrator', NULL), (4, 'u1', 'k1', 'subagent', 2);
    INSERT INTO edges (kind, parent, child, tool_use_id) VALUES ('dispatch', 2, 4, 't1');
    INSERT INTO directs (child, parent, msg_id) VALUES
        ('y', 'd', 'm0'), ('u1', 'd', 'm1'), ('u2', 'd', 'm2');
";

/// Each drawn agent's row id, parent and whether it is of an earlier link.
fn placed(g: &LiveGraph) -> Vec<(i64, Option<i64>, bool)> {
    g.agents
        .iter()
        .map(|a| (a.id, a.parent, a.earlier))
        .collect()
}

#[test]
fn a_unit_chain_directed_in_two_workstreams_draws_each_agent_once() {
    let t = Temp::new("unit-two-workstreams");
    let db = synthetic(&t, TWO_WORKSTREAM_UNIT);
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    // u1 is drawn as itself (ticket-x-2's unit), so k1 hangs under u1's
    // orchestrator, though u2 (ticket-x-1's unit, read first) has u1 as an
    // earlier link.
    assert_eq!(
        placed(&g),
        [
            (1, None, false),
            (3, Some(1), false),
            (2, Some(1), false),
            (4, Some(2), false),
        ]
    );
}

/// The chain pu is l (ticket-x-1, which dispatched k1), a (ticket-x-1) and
/// b (ticket-x-2). Director d directed a, and n: a link of pu with no
/// window yet, so no workstream and no start. n's unit draws the chain's
/// latest link b, whose earlier links in any workstream include l.
const WINDOWLESS_UNIT: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'director-d', 0),
        (2, 'ticket-x-1', 0), (3, 'ticket-x-2', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('d', '/p/d.jsonl', '2026-10-02T09:00:00.000Z', 'd', 1),
        ('l', '/p/l.jsonl', '2026-10-02T10:00:00.000Z', 'pu', 2),
        ('a', '/p/a.jsonl', '2026-10-02T11:00:00.000Z', 'pu', 2),
        ('b', '/p/b.jsonl', '2026-10-02T12:00:00.000Z', 'pu', 3),
        ('n', '/p/n.jsonl', NULL, 'pu', NULL);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'd', 0, '2026-10-02T09:00:00.000Z', 1), (2, 'l', 0, '2026-10-02T10:00:00.000Z', 2),
        (3, 'a', 0, '2026-10-02T11:00:00.000Z', 2), (4, 'b', 0, '2026-10-02T12:00:00.000Z', 3);
    INSERT INTO agents (id, session_id, agent_id, kind, segment_id) VALUES
        (1, 'd', NULL, 'orchestrator', NULL), (2, 'a', NULL, 'orchestrator', NULL),
        (3, 'b', NULL, 'orchestrator', NULL), (4, 'l', 'k1', 'subagent', 2);
    INSERT INTO directs (child, parent, msg_id) VALUES ('a', 'd', 'm1'), ('n', 'd', 'm2');
";

#[test]
fn an_earlier_link_two_units_share_is_drawn_once() {
    let t = Temp::new("unit-windowless");
    let db = synthetic(&t, WINDOWLESS_UNIT);
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    // a's unit (ticket-x-1) draws l's k1 first, and b's does not again.
    assert_eq!(
        placed(&g),
        [
            (1, None, false),
            (2, Some(1), false),
            (4, None, true),
            (3, Some(1), false),
        ]
    );
}

/// One process (pp): p1 (alpha-1, renamed to beta-2 at 10:30), p2
/// (alpha-1) and p3 (beta-2). p1 dispatched k1, which no window holds, and
/// k2 in its alpha-1 window.
const WINDOWLESS_EARLIER: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'alpha-1', 0), (2, 'beta-2', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('p1', '/p/p1.jsonl', '2026-10-02T10:00:00.000Z', 'pp', 2),
        ('p2', '/p/p2.jsonl', '2026-10-02T11:00:00.000Z', 'pp', 1),
        ('p3', '/p/p3.jsonl', '2026-10-02T12:00:00.000Z', 'pp', 2);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'p1', 0, '2026-10-02T10:00:00.000Z', 1), (2, 'p1', 100, '2026-10-02T10:30:00.000Z', 2),
        (3, 'p2', 0, '2026-10-02T11:00:00.000Z', 1), (4, 'p3', 0, '2026-10-02T12:00:00.000Z', 2);
    INSERT INTO agents (id, session_id, agent_id, kind, segment_id) VALUES
        (1, 'p1', NULL, 'orchestrator', NULL), (2, 'p2', NULL, 'orchestrator', NULL),
        (3, 'p3', NULL, 'orchestrator', NULL), (4, 'p1', 'k1', 'subagent', NULL),
        (5, 'p1', 'k2', 'subagent', 1);
    INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
        ('dispatch', 1, 4, 't1'), ('dispatch', 1, 5, 't2');
";

#[test]
fn an_earlier_links_windowless_agent_reads_in_its_links_workstream_only() {
    let t = Temp::new("windowless-earlier");
    let db = synthetic(&t, WINDOWLESS_EARLIER);
    let alpha = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(alpha.session_id, "p2");
    assert_eq!(placed(&alpha), [(2, None, false), (5, Some(2), true)]);
    // k1 reads where p1 ended: beta-2.
    let beta = views::live_graph(&db, 2).expect("query").expect("graph");
    assert_eq!(beta.session_id, "p3");
    assert_eq!(placed(&beta), [(3, None, false), (4, Some(3), true)]);
}

/// Unit chain pk (ticket-x-3): director-a's da directed k1, then
/// director-b's db directed k2. Only db is live.
const TWO_DIRECTORS: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'director-a', 0),
        (2, 'director-b', 0), (3, 'ticket-x-3', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('da', '/p/da.jsonl', '2026-10-02T08:00:00.000Z', 'da', 1),
        ('db', '/p/db.jsonl', '2026-10-02T09:00:00.000Z', 'db', 2),
        ('k1', '/p/k1.jsonl', '2026-10-02T10:00:00.000Z', 'pk', 3),
        ('k2', '/p/k2.jsonl', '2026-10-02T11:00:00.000Z', 'pk', 3);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'da', 0, '2026-10-02T08:00:00.000Z', 1), (2, 'db', 0, '2026-10-02T09:00:00.000Z', 2),
        (3, 'k1', 0, '2026-10-02T10:00:00.000Z', 3), (4, 'k2', 0, '2026-10-02T11:00:00.000Z', 3);
    INSERT INTO directs (child, parent, msg_id) VALUES ('k1', 'da', 'm1'), ('k2', 'db', 'm2');
    INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, first_seen_ms,
        last_seen_ms) VALUES ('pid:2:1', 'interactive', 'db', '/work/x', 1, 'busy', 1, 1);
";

#[test]
fn a_unit_tab_nests_under_a_live_director_of_any_of_its_links() {
    let t = Temp::new("two-directors");
    let db = synthetic(&t, TWO_DIRECTORS);
    let rows = views::workstreams(&db).expect("workstreams");
    // k1's director is not live; k2's is.
    assert_eq!(tab(&rows, "ticket-x-3").director, Some(2));
    // With both live, the earliest directed link's director.
    Connection::open(t.db())
        .expect("raw")
        .execute_batch(
            "INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'da', '/work/x', 1, 'busy', 1, 1);",
        )
        .expect("live");
    let rows = views::workstreams(&db).expect("workstreams");
    assert_eq!(tab(&rows, "ticket-x-3").director, Some(1));
}
