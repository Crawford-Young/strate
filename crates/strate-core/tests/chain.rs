//! A `/clear` chain read as one session (#63), over the synthetic
//! `tests/fixtures/chain-home`: the Audit's one Session row per chain and
//! workstream, a director's `--bg` unit nested under the director's row
//! (which sits under its units' repo), and the live view: a registry row
//! read as its process's latest link (and a hook wait listed on that link
//! only), and the graph drawing a director's live units only, with no
//! finished worker on any link (#81). Every db lives in a temp dir.

use std::path::PathBuf;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::hooks::{Hook, HookEvent};
use strate_core::registry::{RegistryEntry, RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{
    self, AgentState, AuditFilter, AuditNode, GraphAgent, GraphEdge, GroupBy, LiveGraph, Metrics,
    NodeKind, SpanKind, Workstream, audit, audit_timeline,
};

/// director-chain's third and last link.
const L3: &str = "5e55c4c4-0000-4000-8000-0000000000a3";
/// ticket-chain-repo-7's first link, the one director-chain messaged.
const U1: &str = "5e55c4c4-0000-4000-8000-0000000000b1";
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

mod common;

use common::{Temp, batch, fixture_home, transcripts as transcripts_in};

fn home() -> PathBuf {
    fixture_home("chain-home")
}

/// Every transcript under `home/projects`, sorted.
fn transcripts() -> Vec<PathBuf> {
    transcripts_in(&home())
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
    let row = labelled(
        repo(&root, "chain-repo"),
        NodeKind::Session,
        "director-chain",
    );
    // One orchestrator node for the three links, and each link's subagent.
    assert_eq!(agents(row), ["", J[0], J[1], J[2]]);
    let orchestrator = row.children.iter().find(|c| c.key.is_empty()).unwrap();
    // L1 and L3 55 s, L2 121 s: its links, not the hours between them.
    assert_eq!(row.metrics.elapsed_ms, Some((55 + 121 + 55) * S));
    assert_eq!(orchestrator.metrics.elapsed_ms, Some((55 + 121 + 55) * S));
}

#[test]
fn a_directed_unit_and_its_clear_successor_nest_under_the_directors_row() {
    let t = Temp::new("unit");
    let (_store, db) = build(&t);
    let root = tree(&db);
    // Its one unit works in chain-repo: the director's row sits there, at
    // repo level, so nothing is left Unlinked.
    assert!(root.children.iter().all(|c| c.kind != NodeKind::Unlinked));
    let director = labelled(
        repo(&root, "chain-repo"),
        NodeKind::Session,
        "director-chain",
    );
    assert_eq!(labels(director, NodeKind::Session), ["ticket-chain-repo-7"]);
    let unit = labelled(director, NodeKind::Session, "ticket-chain-repo-7");
    // U1 (messaged) and U2 (its /clear successor): one row, both subagents.
    assert_eq!(agents(unit), ["", J[3], J[7]]);
    // U1's run and U2's, without the gap between them.
    assert_eq!(unit.metrics.elapsed_ms, Some(168 * S));
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
    // The unit's work left its own issue: only the chain-repo-5 and -6
    // issues and the director's row sit under the repo.
    let under: Vec<(NodeKind, &str)> = repo(&root, "chain-repo")
        .children
        .iter()
        .map(|c| (c.kind, c.label.as_str()))
        .collect();
    assert_eq!(
        under,
        [
            (NodeKind::Issue, "chain-repo #5"),
            (NodeKind::Issue, "chain-repo #6"),
            (NodeKind::Session, "director-chain"),
        ]
    );
}

/// The Repo child of `node` keyed `slug`.
fn repo<'a>(node: &'a AuditNode, slug: &str) -> &'a AuditNode {
    node.children
        .iter()
        .find(|c| c.kind == NodeKind::Repo && c.key == slug)
        .unwrap_or_else(|| panic!("no repo {slug} under {:?}", node.label))
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
    // P1 and P3, 55 s each: P2's hour between them is chain-repo-6's.
    assert_eq!(five.metrics.elapsed_ms, Some(2 * 55 * S));
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
    let director = labelled(
        repo(&root, "chain-repo"),
        NodeKind::Session,
        "director-chain",
    );
    let unit = labelled(director, NodeKind::Session, "ticket-chain-repo-7");
    let path = vec![
        (NodeKind::Repo, "chain-repo".to_string()),
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
fn entry(pid: u32, session: &str, name: &str) -> RegistryEntry {
    let json = format!(
        r#"[{{"pid":{pid},"cwd":"/work/hub","kind":"interactive","startedAt":1791000000000,
             "sessionId":"{session}","name":"{name}","status":"busy"}}]"#
    );
    parse_snapshot(json.as_bytes()).expect("entry").remove(0)
}

/// That entry, in a poll at 5000.
fn listed(pid: u32, session: &str, name: &str) -> RegistryEvent {
    polled(pid, session, name, 5000)
}

/// That entry, in a poll at `at_ms`.
fn polled(pid: u32, session: &str, name: &str, at_ms: i64) -> RegistryEvent {
    RegistryEvent::Changed {
        entry: entry(pid, session, name),
        at_ms,
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

/// Each drawn agent: its agent id (`-` for an orchestrator) and name.
fn drawn(g: &LiveGraph) -> Vec<(&str, &str)> {
    g.agents
        .iter()
        .map(|a: &GraphAgent| {
            (
                a.agent_id.as_deref().unwrap_or("-"),
                a.name.as_deref().unwrap_or("-"),
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
fn the_director_draws_its_live_units_chain_and_no_finished_subagent() {
    let t = Temp::new("graph");
    let (mut store, db) = build(&t);
    // The unit's process is not running: the canvas leaves it out. Every
    // link's subagent has answered, so none is drawn either.
    let g = graph(&db, "director-chain");
    assert_eq!(g.session_id, L3);
    assert_eq!(drawn(&g), [("-", "director-chain")]);
    // The registry keys a background process by its first session, U1:
    // the unit drawn is its process's latest link, U2.
    store
        .apply_registry(&[listed(4202, U1, "ticket-chain-repo-7")])
        .expect("unit live");
    let g = graph(&db, "director-chain");
    // The unit draws its last link, U2, under the drawn orchestrator.
    assert_eq!(
        drawn(&g),
        [("-", "director-chain"), ("-", "ticket-chain-repo-7"),]
    );
    let id = |i: usize| g.agents[i].id;
    assert_eq!(g.edges, [edge("directs", id(0), id(1))]);
    assert_eq!(
        g.agents.iter().map(|a| a.parent).collect::<Vec<_>>(),
        [None, Some(id(0))]
    );
}

#[test]
fn a_chain_draws_its_latest_link_in_the_drawn_workstream() {
    let t = Temp::new("graph-workstreams");
    let (_store, db) = build(&t);
    // Each link's subagent has answered, so only orchestrators are drawn.
    let five = graph(&db, "chain-repo-5");
    assert_eq!(five.session_id, P3);
    assert_eq!(drawn(&five), [("-", "chain-repo-5")]);
    let six = graph(&db, "chain-repo-6");
    assert_eq!(six.session_id, P2);
    assert_eq!(drawn(&six), [("-", "chain-repo-6")]);
}

#[test]
fn the_header_sums_every_link_the_canvas_draws() {
    let t = Temp::new("header");
    let (_store, db) = build(&t);
    let header = |name| {
        let g = graph(&db, name);
        (g.session_cost_usd.expect("priced"), g.run_ms)
    };
    // L1, L2 (plus its SendMessage) and L3, each with its subagent; each
    // link's run: L1 and L3 :00:00 to :00:55, L2 11:00:00 to 11:02:01.
    let (usd, run) = header("director-chain");
    assert!(close(7.0 * OPUS + 3.0 * SONNET, usd), "{usd}");
    assert_eq!(run, Some((55 + 121 + 55) * S));
    // P1 and P3; P2, renamed to chain-repo-6, reads only there.
    let (usd, run) = header("chain-repo-5");
    assert!(close(4.0 * OPUS + 2.0 * SONNET, usd), "{usd}");
    assert_eq!(run, Some(2 * 55 * S));
    let (usd, run) = header("chain-repo-6");
    assert!(close(2.0 * OPUS + SONNET, usd), "{usd}");
    assert_eq!(run, Some(55 * S));
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
    // d1 has no cell on Oct 2, but its row still reads by its name. The
    // outermost director places the nest: its unit director-mid works
    // where director-mid's own unit does, repo x.
    assert!(root.children.iter().all(|c| c.kind != NodeKind::Unlinked));
    let top = labelled(repo(&root, "x"), NodeKind::Session, "director-top");
    let mid = labelled(top, NodeKind::Session, "director-mid");
    let unit = labelled(mid, NodeKind::Session, "ticket-x-3");
    assert_eq!((top.metrics.sessions, mid.metrics.sessions), (2, 2));
    assert!(close(1.0, unit.metrics.cost_usd));
    // Only the director's row is under x: the unit's work left its issue.
    assert_eq!(repo(&root, "x").children.len(), 1);
}

#[test]
fn rows_that_direct_each_other_nest_one_way_only() {
    let t = Temp::new("cycle");
    let db = synthetic(&t, NESTED);
    let root = oct2(&db);
    check_sums(&root);
    assert!(close(6.0, root.metrics.cost_usd));
    // alpha and beta, besides the directors' x: one of the two holds both.
    let repos: Vec<&AuditNode> = root
        .children
        .iter()
        .filter(|c| c.kind == NodeKind::Repo && c.key != "x")
        .collect();
    assert_eq!(repos.len(), 1, "{:?}", root.children);
    let issue = &repos[0].children[0];
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
/// earlier link in ticket-x-1.
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

/// Each drawn agent's row id and parent.
fn placed(g: &LiveGraph) -> Vec<(i64, Option<i64>)> {
    g.agents.iter().map(|a| (a.id, a.parent)).collect()
}

/// Lists `session` in the registry as a background process (no pid, no
/// status) in `state`.
fn background(t: &Temp, session: &str, state: &str) {
    Connection::open(t.db())
        .expect("raw")
        .execute(
            "INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, state,
                 first_seen_ms, last_seen_ms)
             VALUES ('bg:' || ?1, 'background', ?1, '/work/x', 1, ?2, 1, 1)",
            [session, state],
        )
        .expect("listed");
}

#[test]
fn a_unit_chain_directed_in_two_workstreams_draws_only_its_live_link() {
    let t = Temp::new("unit-two-workstreams");
    let db = synthetic(&t, TWO_WORKSTREAM_UNIT);
    // No unit process runs: the director is drawn alone.
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(placed(&g), [(1, None)]);
    // The registry lists the process by its first link, y: u2, its latest,
    // runs. ticket-x-2's link u1 has finished, so it is not drawn, and nor
    // is its k1 (in ticket-x-1): an earlier link's worker reads done.
    background(&t, "y", "running");
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(placed(&g), [(1, None), (3, Some(1))]);
}

/// The chain pu is l (ticket-x-1, which dispatched k1), a (ticket-x-1) and
/// b (ticket-x-2). Director d directed a, and n: a link of pu with no
/// window yet, so no workstream and no start. n's unit is the chain's
/// latest link b, whose earlier links in any workstream include l and a.
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
fn a_windowless_directed_link_draws_its_chains_live_latest_link() {
    let t = Temp::new("unit-windowless");
    let db = synthetic(&t, WINDOWLESS_UNIT);
    background(&t, "l", "running");
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    // a (ticket-x-1's unit) has finished; b runs. l's k1, an earlier
    // link's worker, reads done and is not drawn.
    assert_eq!(placed(&g), [(1, None), (3, Some(1))]);
}

#[test]
fn a_blocked_background_unit_is_not_drawn() {
    let t = Temp::new("unit-blocked");
    let db = synthetic(&t, TWO_WORKSTREAM_UNIT);
    // Blocked on the user, the unit's process is not running: the canvas
    // leaves it out, as it does a finished one.
    background(&t, "y", "blocked");
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(placed(&g), [(1, None)]);
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

/// One background process (root f): f in ticket-x-17, then /clear and
/// /rename into g, ticket-x-18. The registry keys the process by its first
/// session, f, and has named it ticket-x-18; it waits on a permission.
const RENAMED_PROCESS: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES (1, 'ticket-x-17', 0),
        (2, 'ticket-x-18', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id) VALUES
        ('f', '/p/f.jsonl', '2026-10-02T10:00:00.000Z', 'f', 1),
        ('g', '/p/g.jsonl', '2026-10-02T11:00:00.000Z', 'f', 2);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 'f', 0, '2026-10-02T10:00:00.000Z', 1), (2, 'g', 0, '2026-10-02T11:00:00.000Z', 2);
    INSERT INTO agents (id, session_id, agent_id, kind, name) VALUES
        (1, 'f', NULL, 'orchestrator', 'ticket-x-17'),
        (2, 'g', NULL, 'orchestrator', 'ticket-x-18');
    INSERT INTO registry (key, kind, session_id, name, cwd, started_at_ms, status,
        waiting_for, first_seen_ms, last_seen_ms)
    VALUES ('bg:f', 'background', 'f', 'ticket-x-18', '/work/x', 1, 'waiting', 'Bash', 1, 7);
";

/// The drawn orchestrator's name and state.
fn head(g: &LiveGraph) -> (Option<&str>, AgentState) {
    let a = g
        .agents
        .iter()
        .find(|a| a.agent_id.is_none())
        .expect("head");
    (a.name.as_deref(), a.state)
}

#[test]
fn a_registry_row_is_its_processs_latest_link() {
    let t = Temp::new("registry-latest");
    let db = synthetic(&t, RENAMED_PROCESS);
    let rows = views::workstreams(&db).expect("workstreams");
    let shown = |name: &str| {
        let w = tab(&rows, name);
        (w.session_id.as_deref(), w.live, w.needs_you, w.state)
    };
    // The tab is the running link's workstream; the finished ticket's is not.
    assert_eq!(
        shown("ticket-x-18"),
        (Some("g"), true, 1, AgentState::NeedsYou)
    );
    assert_eq!(
        shown("ticket-x-17"),
        (Some("f"), false, 0, AgentState::Done)
    );
    // The wait names the latest link and counts toward its workstream.
    let needs: Vec<(Option<String>, Option<i64>)> = views::needs_you(&db)
        .expect("needs you")
        .into_iter()
        .map(|n| (n.session_id, n.workstream))
        .collect();
    assert_eq!(needs, [(Some("g".to_string()), Some(2))]);
    let g = views::live_graph(&db, 2).expect("query").expect("graph");
    assert!(g.live);
    assert_eq!(head(&g), (Some("ticket-x-18"), AgentState::NeedsYou));
    let f = views::live_graph(&db, 1).expect("query").expect("graph");
    assert!(!f.live);
    assert_eq!(head(&f), (Some("ticket-x-17"), AgentState::Done));

    Connection::open(t.db())
        .expect("raw")
        .execute(
            "UPDATE registry SET status = 'busy', waiting_for = NULL",
            [],
        )
        .expect("busy");
    let rows = views::workstreams(&db).expect("workstreams");
    assert_eq!(tab(&rows, "ticket-x-18").state, AgentState::Working);
    let g = views::live_graph(&db, 2).expect("query").expect("graph");
    assert_eq!(head(&g), (Some("ticket-x-18"), AgentState::Working));
}

/// Six directors on Oct 2, each session one $1 orchestrator cell, each
/// unit messaged from its director's one window:
/// - director-xrepo (d1): ticket-xrepo-1 (u1), ticket-yrepo-2 (u2);
/// - director-wrepo (d2): ticket-xrepo-3 (u3), ticket-yrepo-4 (u4), where
///   wrepo is a repo only the GitHub cache knows;
/// - director-zz (d3): ticket-xrepo-5 (u5), ticket-yrepo-6 (u6);
/// - director-solo (d4): ticket-yrepo-7 (u7), and u8, whose name (a pasted
///   launch line) names no repo;
/// - director-yrepo (d5) and director-qq (d6), with no units.
const PLACED: &str = "
    INSERT INTO workstreams (id, name, fallback) VALUES
        (1, 'director-xrepo', 0), (2, 'director-wrepo', 0), (3, 'director-zz', 0),
        (4, 'director-solo', 0), (11, 'ticket-xrepo-1', 0), (12, 'ticket-yrepo-2', 0),
        (13, 'ticket-xrepo-3', 0), (14, 'ticket-yrepo-4', 0), (15, 'ticket-xrepo-5', 0),
        (16, 'ticket-yrepo-6', 0), (17, 'ticket-yrepo-7', 0), (18, 'claude -n ticket-yrepo-8', 0),
        (5, 'director-yrepo', 0), (6, 'director-qq', 0);
    INSERT INTO sessions (session_id, path, started_at, process_root, workstream_id)
    SELECT s, '/p/' || s || '.jsonl', '2026-10-02T1' || (w % 10) || ':00:00.000Z', s, w
    FROM (SELECT 'd1' AS s, 1 AS w UNION ALL SELECT 'd2', 2 UNION ALL SELECT 'd3', 3
        UNION ALL SELECT 'd4', 4 UNION ALL SELECT 'u1', 11 UNION ALL SELECT 'u2', 12
        UNION ALL SELECT 'u3', 13 UNION ALL SELECT 'u4', 14 UNION ALL SELECT 'u5', 15
        UNION ALL SELECT 'u6', 16 UNION ALL SELECT 'u7', 17 UNION ALL SELECT 'u8', 18
        UNION ALL SELECT 'd5', 5 UNION ALL SELECT 'd6', 6);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id)
    SELECT workstream_id, session_id, 0, started_at, workstream_id FROM sessions;
    INSERT INTO events (session_id, file, byte_offset, raw, sent_msg_id)
    SELECT p, '/p/' || p || '.jsonl', o, '{}', 'm-' || c
    FROM (SELECT 'd1' AS p, 'u1' AS c, 10 AS o UNION ALL SELECT 'd1', 'u2', 20
        UNION ALL SELECT 'd2', 'u3', 10 UNION ALL SELECT 'd2', 'u4', 20
        UNION ALL SELECT 'd3', 'u5', 10 UNION ALL SELECT 'd3', 'u6', 20
        UNION ALL SELECT 'd4', 'u7', 10 UNION ALL SELECT 'd4', 'u8', 20);
    INSERT INTO directs (child, parent, msg_id)
    SELECT substr(sent_msg_id, 3), session_id, sent_msg_id FROM events;
    INSERT INTO gh_repos (slug, owner, name, fetched_at) VALUES ('wrepo', 'o', 'wrepo', 1);
    INSERT INTO audit_cells (session_id, agent_id, segment_id, day, model, wall_ms,
        active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens)
    SELECT session_id, NULL, workstream_id, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1,
        0, 0
    FROM sessions;
";

/// The tree on Oct 2 with only `repos`' windows.
fn oct2_in(db: &Connection, repos: &[&str]) -> AuditNode {
    let f = AuditFilter {
        from: "2026-10-02".into(),
        to: "2026-10-02".into(),
        repos: repos.iter().map(|r| r.to_string()).collect(),
        group_by: GroupBy::Hierarchy,
    };
    audit(db, &f).expect("audit").root
}

/// A top-level node's kind and key, and each Session row under it: its
/// label, $ and the labels of the rows nested in it.
type Placement<'a> = (NodeKind, &'a str, Vec<(&'a str, f64, Vec<&'a str>)>);

fn placements(root: &AuditNode) -> Vec<Placement<'_>> {
    root.children
        .iter()
        .map(|place| {
            let rows = place
                .children
                .iter()
                .map(|row| {
                    assert_eq!(row.kind, NodeKind::Session, "{:?}", row.label);
                    (
                        row.label.as_str(),
                        row.metrics.cost_usd,
                        labels(row, NodeKind::Session),
                    )
                })
                .collect();
            (place.kind, place.key.as_str(), rows)
        })
        .collect()
}

#[test]
fn a_directors_row_sits_under_its_units_repo_else_the_repo_its_name_names() {
    let t = Temp::new("placed");
    let db = synthetic(&t, PLACED);
    let root = oct2_in(&db, &[]);
    check_sums(&root);
    assert!(close(14.0, root.metrics.cost_usd));
    assert_eq!(root.metrics.sessions, 14);
    assert_eq!(
        placements(&root),
        [
            // Units in two repos: the repo the name names, known from the
            // GitHub cache (wrepo) or from window names (xrepo).
            (
                NodeKind::Repo,
                "wrepo",
                vec![(
                    "director-wrepo",
                    3.0,
                    vec!["ticket-xrepo-3", "ticket-yrepo-4"]
                )]
            ),
            (
                NodeKind::Repo,
                "xrepo",
                vec![(
                    "director-xrepo",
                    3.0,
                    vec!["ticket-xrepo-1", "ticket-yrepo-2"]
                )]
            ),
            // All its units that name a repo name one: that repo, whatever
            // the name. No units: the repo the name names.
            (
                NodeKind::Repo,
                "yrepo",
                vec![
                    (
                        "director-solo",
                        3.0,
                        vec!["ticket-yrepo-7", "claude -n ticket-yrepo-8"]
                    ),
                    ("director-yrepo", 1.0, vec![]),
                ]
            ),
            // Units in two repos, or none, and a name naming no known repo.
            (
                NodeKind::Unlinked,
                "",
                vec![
                    ("director-zz", 3.0, vec!["ticket-xrepo-5", "ticket-yrepo-6"]),
                    ("director-qq", 1.0, vec![]),
                ]
            ),
        ]
    );
}

#[test]
fn the_repo_filter_keeps_a_directors_own_work_under_its_placement() {
    let t = Temp::new("placed-filter");
    let db = synthetic(&t, PLACED);
    // A window's work is kept when its own repo is filtered: a director's
    // where its row is placed, a unit's by its own name. A unit's work
    // still nests under its director, wherever that director sits.
    let root = oct2_in(&db, &["xrepo"]);
    check_sums(&root);
    assert_eq!(
        placements(&root),
        [
            (
                NodeKind::Repo,
                "wrepo",
                vec![("director-wrepo", 1.0, vec!["ticket-xrepo-3"])]
            ),
            (
                NodeKind::Repo,
                "xrepo",
                vec![("director-xrepo", 2.0, vec!["ticket-xrepo-1"])]
            ),
            (
                NodeKind::Unlinked,
                "",
                vec![("director-zz", 1.0, vec!["ticket-xrepo-5"])]
            ),
        ]
    );
    let root = oct2_in(&db, &["yrepo"]);
    check_sums(&root);
    let yrepo = repo(&root, "yrepo");
    assert_eq!(
        labels(yrepo, NodeKind::Session),
        ["director-solo", "director-yrepo"]
    );
    assert!(close(3.0, yrepo.metrics.cost_usd));
    assert!(close(6.0, root.metrics.cost_usd));
}

#[test]
fn a_blocked_background_session_has_no_tab_but_needs_you() {
    let t = Temp::new("registry-blocked");
    let db = synthetic(&t, RENAMED_PROCESS);
    // The CLI lists a background session waiting on the user as state
    // blocked, with no pid, status or waitingFor.
    Connection::open(t.db())
        .expect("raw")
        .execute(
            "UPDATE registry SET status = NULL, waiting_for = NULL, state = 'blocked'",
            [],
        )
        .expect("blocked");
    let rows = views::workstreams(&db).expect("workstreams");
    let shown = |name: &str| {
        let w = tab(&rows, name);
        (w.session_id.as_deref(), w.live, w.needs_you, w.state)
    };
    // No tab: neither link is live. The wait counts toward the latest
    // link's workstream.
    assert_eq!(
        shown("ticket-x-18"),
        (Some("g"), false, 1, AgentState::NeedsYou)
    );
    assert_eq!(
        shown("ticket-x-17"),
        (Some("f"), false, 0, AgentState::Done)
    );
    let needs: Vec<(String, Option<String>, Option<i64>)> = views::needs_you(&db)
        .expect("needs you")
        .into_iter()
        .map(|n| (n.source, n.session_id, n.workstream))
        .collect();
    assert_eq!(
        needs,
        [("registry".to_string(), Some("g".to_string()), Some(2))]
    );
    let g = views::live_graph(&db, 2).expect("query").expect("graph");
    assert!(!g.live);
    assert_eq!(head(&g), (Some("ticket-x-18"), AgentState::NeedsYou));
}

/// A PermissionRequest hook on `session`'s orchestrator, or on its
/// subagent `agent`, received at `at_ms`.
fn asks(session: &str, agent: Option<&str>, at_ms: i64) -> Hook {
    Hook {
        received_ms: at_ms,
        event: HookEvent::PermissionRequest {
            session_id: session.into(),
            agent_id: agent.map(Into::into),
            tool_name: Some("Bash".into()),
        },
    }
}

/// Each listed hook wait: its session and agent id.
fn hook_waits(db: &Connection) -> Vec<(Option<String>, Option<String>)> {
    views::needs_you(db)
        .expect("needs you")
        .into_iter()
        .filter(|n| n.source == "hook")
        .map(|n| (n.session_id, n.agent_id))
        .collect()
}

#[test]
fn a_hook_wait_is_listed_only_on_its_processs_latest_link() {
    let t = Temp::new("hook-latest");
    let (mut store, db) = build(&t);
    // U1's prompts were left behind when the process /cleared into U2.
    store.apply_hook(&asks(U1, None, 6000)).expect("U1 asks");
    store
        .apply_hook(&asks(U1, Some(J[3]), 6000))
        .expect("J4 asks");
    assert_eq!(hook_waits(&db), [], "no registry row lists the process");
    // The registry keys the process by U1: still only U2's prompt counts.
    store
        .apply_registry(&[listed(4202, U1, "ticket-chain-repo-7")])
        .expect("unit live");
    store
        .apply_hook(&asks(U2, Some(J[7]), 7000))
        .expect("J8 asks");
    assert_eq!(hook_waits(&db), [(Some(U2.into()), Some(J[7].into()))]);
    let rows = views::workstreams(&db).expect("workstreams");
    assert_eq!(tab(&rows, "ticket-chain-repo-7").needs_you, 1);
    // The director's canvas draws U2's waiting subagent, not U1's done one.
    let g = graph(&db, "director-chain");
    let state = |id: &str| {
        g.agents
            .iter()
            .find(|a| a.agent_id.as_deref() == Some(id))
            .map(|a| a.state)
    };
    assert_eq!(state(J[3]), None);
    assert_eq!(state(J[7]), Some(AgentState::NeedsYou));
}

/// Each orchestrator permission wait: its session, start and end.
fn orchestrator_waits(db: &Connection) -> Vec<(String, i64, Option<i64>)> {
    db.prepare(
        "SELECT a.session_id, w.start_ms, w.end_ms
         FROM permission_waits AS w JOIN agents AS a ON a.id = w.agent
         WHERE a.agent_id IS NULL ORDER BY w.start_ms, a.session_id",
    )
    .expect("query")
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .expect("rows")
    .collect::<rusqlite::Result<_>>()
    .expect("waits")
}

#[test]
fn the_registry_answers_and_ends_its_processs_latest_links_prompt() {
    let t = Temp::new("registry-chain-prompt");
    let (mut store, db) = build(&t);
    store
        .apply_registry(&[listed(4202, U1, "ticket-chain-repo-7")])
        .expect("unit live");
    store.apply_hook(&asks(U1, None, 6000)).expect("U1 asks");
    store.apply_hook(&asks(U2, None, 6000)).expect("U2 asks");
    // A poll at or after the request that is not waiting answers the
    // prompt on the process's latest link. U1's went with the /clear: the
    // view leaves it out, and the registry leaves it be.
    store
        .apply_registry(&[polled(4202, U1, "ticket-chain-repo-7", 7000)])
        .expect("answered");
    assert_eq!(hook_waits(&db), []);
    assert_eq!(
        orchestrator_waits(&db),
        [(U1.into(), 6000, None), (U2.into(), 6000, Some(7000))]
    );
    // Once the registry stops listing the process, its prompt is gone.
    store
        .apply_hook(&asks(U2, None, 8000))
        .expect("U2 asks again");
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry: entry(4202, U1, "ticket-chain-repo-7"),
            last_seen_ms: 9000,
        }])
        .expect("gone");
    assert_eq!(hook_waits(&db), []);
    assert_eq!(orchestrator_waits(&db)[2], (U2.into(), 8000, Some(9000)));
}

/// The cold build without `session`'s transcripts (its orchestrator's and
/// its subagents'), and a reader on it.
fn build_without(t: &Temp, session: &str) -> (Store, Connection) {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&home()).expect("discover"))
        .expect("graph");
    for path in transcripts() {
        if !path.to_string_lossy().contains(session) {
            store.ingest(&batch(&path)).expect("ingest");
        }
    }
    let reader = Store::open_reader(t.db()).expect("reader");
    (store, reader)
}

/// Ingests `session`'s transcripts (its orchestrator's, then its
/// subagents').
fn ingest_link(store: &mut Store, session: &str) {
    for path in transcripts() {
        if path.to_string_lossy().contains(session) {
            store.ingest(&batch(&path)).expect("ingest");
        }
    }
}

/// Each permission wait: its session, agent id, start and end.
fn waits(db: &Connection) -> Vec<(String, Option<String>, i64, Option<i64>)> {
    db.prepare(
        "SELECT a.session_id, a.agent_id, w.start_ms, w.end_ms
         FROM permission_waits AS w JOIN agents AS a ON a.id = w.agent
         ORDER BY w.start_ms, a.session_id, a.agent_id",
    )
    .expect("query")
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    .expect("rows")
    .collect::<rusqlite::Result<_>>()
    .expect("waits")
}

#[test]
fn an_earlier_links_open_waits_end_at_the_next_links_first_record() {
    let t = Temp::new("earlier-waits");
    let (mut store, db) = build_without(&t, U2);
    // 2026-10-08: U1 asks at 11:10, after its last record (11:03:55); so
    // does P2, of another process. U2's first record is 11:30:00.
    let (asked, next, after) = (1_791_457_800_000, 1_791_459_000_000, 1_791_459_060_000);
    store.apply_hook(&asks(U1, None, asked)).expect("U1 asks");
    store.apply_hook(&asks(P2, None, asked)).expect("P2 asks");
    // J4's request came after U2 began: not one the /clear took.
    store
        .apply_hook(&asks(U1, Some(J[3]), after))
        .expect("J4 asks");
    ingest_link(&mut store, U2);
    assert_eq!(
        waits(&db),
        [
            (U1.into(), None, asked, Some(next)),
            (P2.into(), None, asked, None),
            (U1.into(), Some(J[3].into()), after, None),
        ]
    );
    let state: Option<String> = db
        .query_row(
            "SELECT hook_state FROM agents WHERE session_id = ?1 AND agent_id IS NULL",
            [U1],
            |r| r.get(0),
        )
        .expect("U1");
    assert_eq!(state, None, "an ended wait's agent no longer needs you");
}

#[test]
fn an_earlier_links_subagent_wait_ends_with_its_link() {
    let t = Temp::new("earlier-subagent-wait");
    let (mut store, db) = build_without(&t, U2);
    let (asked, next) = (1_791_457_800_000, 1_791_459_000_000);
    store
        .apply_hook(&asks(U1, Some(J[3]), asked))
        .expect("J4 asks");
    ingest_link(&mut store, U2);
    assert_eq!(
        waits(&db),
        [(U1.into(), Some(J[3].into()), asked, Some(next))]
    );
    // A later U2 batch leaves its own link's wait be: U2 is the latest.
    store
        .apply_hook(&asks(U2, None, next + 70_000))
        .expect("U2 asks");
    ingest_link(&mut store, U2);
    assert_eq!(waits(&db)[1], (U2.into(), None, next + 70_000, None));
}
