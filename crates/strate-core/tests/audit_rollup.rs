//! Audit rollups (#26) over the synthetic `tests/fixtures/audit-home` and its
//! GitHub data (`tests/fixtures/github/audit-home.json`), with days in
//! America/Toronto. Hand values for the closed issue `demo-9`, a property
//! test (every additive metric of every node is the sum of its children,
//! and the root is the sum of the cells in the filter), the splits by day,
//! agent type and model, the date-range and repo filters, Unlinked and the
//! headline. Each charge kind's path (#107) over a synthetic store, and
//! `tests/fixtures/model-home`: a subagent charged apart from its root
//! agent, a topic, a messaged session and Unlinked. The charge tree (#110):
//! root-agent and subagent nodes under each placed node, and the split by
//! role. The row
//! timeline (`audit_timeline`): hand values for demo-9 across a local
//! midnight, and a property test (each day's spans sum to every node's
//! cells that day). A nested epic (#47) over
//! `tests/fixtures/nest-home` and `github/nest-home.json`. Every db lives in
//! a temp dir.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::github::{Runner, fetch_repo, refresh};
use strate_core::registry::{Kind, RegistryEntry, RegistryEvent};
use strate_core::store::Store;
use strate_core::store::views::{
    Audit, AuditFilter, AuditNode, Metrics, NodeKind, SpanKind, TimelineDay, TimelineMark,
    TimelineSpan, audit, audit_timeline,
};

const TORONTO: &str = "America/Toronto";
/// demo-9 and issue_demo-9: the closed issue's two sessions.
const A1: &str = "5e559a9a-0000-4000-8000-0000000000a1";
const A2: &str = "5e559a9a-0000-4000-8000-0000000000a2";
/// nest-5, with a background subagent.
const N4: &str = "5e55a7a7-0000-4000-8000-0000000000b4";
const FG: &str = "a9000000000000001";
const BG: &str = "a9000000000000002";
const S: i64 = 1000;

mod common;

use common::{Temp, batch, fixture_home, fixtures, transcripts};

/// A runner answering each `gh` call with the next fixture file.
fn scripted(files: &[&str]) -> Runner {
    let mut script: VecDeque<PathBuf> = files
        .iter()
        .map(|f| fixtures().join("github").join(f))
        .collect();
    Box::new(move |_| match script.pop_front() {
        Some(path) => fs::read(path),
        None => Err(io::Error::other("script exhausted")),
    })
}

/// Cold build in Toronto: the discovery graph, each transcript as one batch,
/// then the GitHub cache. `demo` resolves its owner from the pr-link and
/// fetches the fixture; `other` asks for the login, then is not found.
fn build(t: &Temp) -> Connection {
    let mut store = build_without_gh(t, "audit-home");
    let mut runner = scripted(&["audit-home.json", "user.json", "not-found.json"]);
    refresh(&mut store, &mut runner, &["demo", "other"], 1).expect("gh refresh");
    Store::open_reader(t.db()).expect("reader")
}

/// Cold build of `tests/fixtures/nest-home` in Toronto: `nest` asks for the
/// login, then fetches the fixture.
fn build_nest(t: &Temp) -> Connection {
    let mut store = build_without_gh(t, "nest-home");
    let mut runner = scripted(&["user.json", "nest-home.json"]);
    refresh(&mut store, &mut runner, &["nest"], 1).expect("gh refresh");
    Store::open_reader(t.db()).expect("reader")
}

/// The build of fixture home `name` with no GitHub refresh: an empty cache,
/// as with no `gh`.
fn build_without_gh(t: &Temp, name: &str) -> Store {
    let home = fixture_home(name);
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    store
        .ingest_graph(&discover(&home).expect("discover"))
        .expect("graph");
    for path in transcripts(&home) {
        store.ingest(&batch(&path)).expect("ingest");
    }
    store
}

fn filter(from: &str, to: &str, repos: &[&str]) -> AuditFilter {
    AuditFilter {
        from: from.into(),
        to: to.into(),
        repos: repos.iter().map(|r| r.to_string()).collect(),
    }
}

fn october(db: &Connection) -> Audit {
    audit(db, &filter("2026-10-01", "2026-10-31", &[])).expect("audit")
}

/// The child of `node` of `kind` keyed `key`.
fn child<'a>(node: &'a AuditNode, kind: NodeKind, key: &str) -> &'a AuditNode {
    node.children
        .iter()
        .find(|c| c.kind == kind && c.key == key)
        .unwrap_or_else(|| {
            let have: Vec<_> = node.children.iter().map(|c| (c.kind, &c.key)).collect();
            panic!("no {kind:?} {key:?} under {:?}; have {have:?}", node.key)
        })
}

/// The child of `node` of `kind` labelled `label`.
fn labelled<'a>(node: &'a AuditNode, kind: NodeKind, label: &str) -> &'a AuditNode {
    node.children
        .iter()
        .find(|c| c.kind == kind && c.label == label)
        .unwrap_or_else(|| panic!("no {kind:?} labelled {label:?} under {:?}", node.key))
}

/// Repo demo -> epic #8 -> issue `n`.
fn issue<'a>(root: &'a AuditNode, n: &str) -> &'a AuditNode {
    let epic = child(child(root, NodeKind::Repo, "demo"), NodeKind::Epic, "8");
    child(epic, NodeKind::Issue, n)
}

fn close(want: f64, got: f64) -> bool {
    (want - got).abs() < 1e-9
}

/// Opus 5.5 is $4 in / $20 out / $5 cache write / $0.2 cache read per MTok,
/// Sonnet 5.5 $2 / $10 / - / $0.1. Every opus request in the fixture is
/// 1000 in, 100 out, 10000 read: (4000 + 2000 + 2000) / 1e6 = $0.008; req_a1
/// also writes 2000 (5m): + 10000 / 1e6 = $0.018. Every sonnet request is
/// 500 in, 200 out, 5000 read: (1000 + 2000 + 500) / 1e6 = $0.0035.
const OPUS: f64 = 0.008;
const SONNET: f64 = 0.0035;

#[test]
fn the_closed_issue_matches_its_hand_values() {
    let t = Temp::new("hand");
    let db = build(&t);
    let tree = october(&db).root;
    let demo9 = issue(&tree, "9");
    let m = &demo9.metrics;
    // Two sessions, each its own transcript (times Z, Toronto = Z - 4 h):
    // demo-9 (14:00:00..14:05:20 on Oct 4), relative seconds: prompt 0,
    // req_a1 10 (Agent toolu_a1), result 120, req_a2 130, prompt 310, req_a3
    // 320; its subagent at 20 and 80. Model [0,10] + [120,130] + [310,320],
    // tools [10,120], wait for the 310 prompt since the last work [130,310]
    // = 180 s; no gap over 10 min: active 320 s.
    // issue_demo-9 (03:50:00..04:05:10 on Oct 5): prompt 0, req_b1 10 (Agent,
    // background), result 15, req_b2 25, prompt 900, req_b3 910; its subagent
    // at 20 and 140. Model [0,10] + [15,25] + [900,910], tools [10,15], wait
    // [25,140] (the wait [25,900] loses [140,900] to idle: 760 s > 10 min).
    // Active 910 - 760 = 150 s, waiting 115 s.
    assert_eq!(m.active_ms, (320 + 150) * S);
    assert_eq!(m.wait_ms, (180 + 115) * S);
    // Subagents: [20,80] = 60 s and [20,140] = 120 s.
    assert_eq!(m.agent_ms, (60 + 120) * S);
    // Opus: req_a1 + a2 + a3 + b1 + b2 + b3; sonnet: f1 + g1.
    let usd = 0.018 + 5.0 * OPUS + 2.0 * SONNET;
    assert!(close(0.065, usd));
    assert!(close(usd, m.cost_usd), "{}", m.cost_usd);
    assert_eq!(m.unpriced, 0);
    let tk = &m.tokens;
    // 6 opus (1000 in, 100 out, 10000 read) + 2 sonnet (500, 200, 5000);
    // req_a1's 2000 cache write.
    assert_eq!(
        (tk.input, tk.output, tk.cache_read, tk.cache_write),
        (7000, 1000, 70_000, 2000)
    );
    assert_eq!((m.sessions, m.agents), (2, 2));
    // First session start Oct 4 14:00:00Z -> closed Oct 5 12:00:00Z = 22 h.
    assert_eq!(m.elapsed_ms, Some(22 * 3600 * S));
    assert!(!m.open);
    assert_eq!(m.progress, None);

    // Epic #8: 1 of its 2 sub-issues closed; still open.
    let epic = child(child(&tree, NodeKind::Repo, "demo"), NodeKind::Epic, "8");
    let p = epic.metrics.progress.as_ref().expect("epic progress");
    assert_eq!((p.completed, p.total), (1, 2));
    assert!(epic.metrics.open);
    // Oct 4 14:00:00Z to #10's last activity (Oct 5 15:00:30Z): 25 h 30 s.
    assert_eq!(epic.metrics.elapsed_ms, Some((25 * 3600 + 30) * S));
    assert_eq!(epic.label, "#8 Epic: lorem audit");
}

/// Whether `node` is a subagent's Agent node: it counts its one subagent,
/// where a root agent counts none.
fn is_subagent(node: &AuditNode) -> bool {
    node.kind == NodeKind::Agent && node.metrics.agents > 0
}

#[test]
fn root_agents_and_subagents_sit_under_their_pr_or_directly_under_the_issue() {
    use NodeKind::Agent;
    let t = Temp::new("levels");
    let db = build(&t);
    let tree = october(&db).root;
    let demo9 = issue(&tree, "9");
    assert_eq!(demo9.label, "#9 Ipsum rollup");
    // issue_demo-9 links PR #12; demo-9 falls back to the issue's only PR.
    assert_eq!(demo9.children.len(), 1);
    let pr = child(demo9, NodeKind::Pr, "12");
    assert!(!pr.metrics.open, "merged");
    // First session start Oct 4 14:00:00Z -> merged Oct 5 11:00:00Z.
    assert_eq!(pr.metrics.elapsed_ms, Some(21 * 3600 * S));
    // No Session level: each chain's root agent (keyed by its chain and
    // charge), then that chain's subagents (keyed by session and agent),
    // chains in start order.
    let (first_key, second_key) = (format!("{A1}/issue_demo-9"), format!("{A2}/issue_demo-9"));
    let (fg, bg) = (format!("{A1}/{FG}"), format!("{A2}/{BG}"));
    assert_eq!(
        kinds(pr),
        [
            (Agent, first_key.as_str()),
            (Agent, fg.as_str()),
            (Agent, second_key.as_str()),
            (Agent, bg.as_str())
        ]
    );
    let first = child(pr, Agent, &first_key);
    let second = child(pr, Agent, &second_key);
    assert_eq!(
        (first.label.as_str(), second.label.as_str()),
        ("demo-9", "issue_demo-9")
    );
    assert!(first.children.is_empty() && second.children.is_empty());
    // A root agent's elapsed is its sessions' time.
    assert_eq!(first.metrics.elapsed_ms, Some(320 * S));
    assert_eq!(second.metrics.elapsed_ms, Some(910 * S));
    assert_eq!(
        (
            first.metrics.sessions,
            first.metrics.agents,
            first.metrics.agent_ms
        ),
        (1, 0, 0)
    );

    // Each Agent node keeps its own active and waiting time.
    let sub = child(pr, Agent, &fg);
    assert_eq!(first.metrics.active_ms, 320 * S);
    assert_eq!(
        (sub.metrics.active_ms, sub.metrics.agent_ms),
        (60 * S, 60 * S)
    );
    assert_eq!(sub.metrics.elapsed_ms, Some(60 * S));
    assert_eq!(sub.metrics.agents, 1);
    // The PR sums its root agents' active time, not its subagents' too.
    assert_eq!(pr.metrics.active_ms, (320 + 150) * S, "not + 60 + 120");

    // #10 is open with no PR: its root agent sits directly under it.
    let demo10 = issue(&tree, "10");
    let window = labelled(demo10, Agent, "demo-10");
    assert!(demo10.metrics.open);
    assert_eq!(demo10.metrics.elapsed_ms, Some(30 * S));
    assert!(close(OPUS, window.metrics.cost_usd));
}

#[test]
fn an_epic_with_its_own_session_is_one_node_with_direct_work() {
    let t = Temp::new("epic-direct");
    let db = build(&t);
    let tree = october(&db).root;
    let demo = child(&tree, NodeKind::Repo, "demo");
    // #8 is an epic, so its session is not an `Issue 8` beside it.
    let kinds: Vec<_> = demo
        .children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect();
    assert_eq!(kinds, [(NodeKind::Epic, "8")]);
    let epic = child(demo, NodeKind::Epic, "8");
    let keys: Vec<_> = epic.children.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["8", "9", "10"]);
    let direct = child(epic, NodeKind::Issue, "8");
    assert_eq!(direct.label, "Direct work");
    assert!(
        direct.metrics.progress.is_none(),
        "progress stays on the epic"
    );
    let window = labelled(direct, NodeKind::Agent, "demo-8");
    // 10:00:00Z -> 10:00:20Z, one opus request.
    assert_eq!(window.metrics.elapsed_ms, Some(20 * S));
    assert!(close(OPUS, direct.metrics.cost_usd));
    assert!(close(OPUS, window.metrics.cost_usd));
    assert_eq!((direct.metrics.sessions, direct.metrics.agents), (1, 0));
    // The epic still sums its direct work and both sub-issues.
    let sub = |n| child(epic, NodeKind::Issue, n).metrics.cost_usd;
    assert!(close(
        sub("8") + sub("9") + sub("10"),
        epic.metrics.cost_usd
    ));
    check_all_sums(&db);
}

/// How many nodes under `node`, itself included, are `kind` keyed `key`.
fn count(node: &AuditNode, kind: NodeKind, key: &str) -> usize {
    let own = usize::from(node.kind == kind && node.key == key);
    own + node
        .children
        .iter()
        .map(|c| count(c, kind, key))
        .sum::<usize>()
}

/// The `(kind, key)` of each child of `node`, in order.
fn kinds(node: &AuditNode) -> Vec<(NodeKind, &str)> {
    node.children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect()
}

#[test]
fn a_nested_epic_is_one_node_under_its_parent() {
    let t = Temp::new("nested-epic");
    let db = build_nest(&t);
    let tree = october(&db).root;
    let nest = child(&tree, NodeKind::Repo, "nest");
    // #5's only sub-issue is in another repo: an issue, not an epic whose
    // only child is its direct work.
    assert_eq!(kinds(nest), [(NodeKind::Epic, "1"), (NodeKind::Issue, "5")]);
    let five = child(nest, NodeKind::Issue, "5");
    assert_eq!(five.label, "#5 Amet abroad");
    labelled(five, NodeKind::Agent, "nest-5");

    // #2 is a sub-issue of #1 and the epic over #3: one Epic node under #1.
    let one = child(nest, NodeKind::Epic, "1");
    assert_eq!(kinds(one), [(NodeKind::Epic, "2"), (NodeKind::Issue, "4")]);
    assert_eq!(count(&tree, NodeKind::Epic, "2"), 1);
    assert_eq!(
        count(&tree, NodeKind::Issue, "2"),
        1,
        "its direct work only"
    );
    let two = child(one, NodeKind::Epic, "2");
    assert_eq!(two.label, "#2 Nested ipsum");
    let p = two.metrics.progress.as_ref().expect("nested epic progress");
    assert_eq!((p.completed, p.total), (1, 1));
    assert_eq!(kinds(two), [(NodeKind::Issue, "2"), (NodeKind::Issue, "3")]);
    let direct = child(two, NodeKind::Issue, "2");
    assert_eq!(direct.label, "Direct work");
    assert!(direct.metrics.progress.is_none());
    labelled(direct, NodeKind::Agent, "nest-2");
    labelled(child(two, NodeKind::Issue, "3"), NodeKind::Agent, "nest-3");

    // One opus request per session: #2 holds its own and #3's, #1 also #4's.
    assert!(close(2.0 * OPUS, two.metrics.cost_usd));
    assert!(close(3.0 * OPUS, one.metrics.cost_usd));
    assert_eq!((two.metrics.sessions, one.metrics.sessions), (2, 3));
    // Open: nest-2's start (Oct 4 14:00:00Z) to #3's close (Oct 5
    // 18:00:00Z), its latest child end.
    assert!(two.metrics.open);
    assert_eq!(two.metrics.elapsed_ms, Some(28 * 3600 * S));

    // The timeline finds the nested epic by the same path.
    let oct = filter("2026-10-01", "2026-10-31", &[]);
    let to_two = [
        (NodeKind::Repo, "nest"),
        (NodeKind::Epic, "1"),
        (NodeKind::Epic, "2"),
    ];
    let days = audit_timeline(&db, &oct, &steps(&to_two)).expect("timeline");
    let on: Vec<_> = days.iter().map(|d| d.day.as_str()).collect();
    assert_eq!(on, ["2026-10-04", "2026-10-05"]);
    check_all_sums(&db);
}

#[test]
fn a_parent_cycle_in_the_cache_stops_at_the_first_repeat() {
    let t = Temp::new("parent-cycle");
    // GitHub allows no cycle, but a refresh paging while an issue moves
    // could cache one: #1 under #2 under #1.
    let db = synthetic(
        &t,
        "INSERT INTO sessions (session_id, path, started_at) VALUES
             ('s1', '/p/s1.jsonl', '2026-10-02T10:00:00.000Z');
         INSERT INTO charges (id, key, kind, repo, number) VALUES
             (1, 'issue_ring-1', 'issue', 'ring', 1);
         INSERT INTO agents (session_id, kind, own_charge, own_charge_source)
             VALUES ('s1', 'orchestrator', 1, 'name');
         INSERT INTO audit_cells (session_id, agent_id, day, model, wall_ms,
             active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
             cache_read_tokens, cache_write_tokens) VALUES
             ('s1', NULL, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0);
         INSERT INTO gh_repos (slug, owner, name, fetched_at) VALUES ('ring', 'example', 'ring', 1);
         INSERT INTO gh_issues (repo, number, title, state, created_at, parent, sub_total,
             sub_completed) VALUES
             ('ring', 1, 'Lorem', 'OPEN', '2026-10-01T00:00:00Z', 2, 1, 0),
             ('ring', 2, 'Ipsum', 'OPEN', '2026-10-01T00:00:00Z', 1, 1, 0);",
    );
    let tree = lab(&db, "2026-10-01", "2026-10-31").root;
    let two = child(child(&tree, NodeKind::Repo, "ring"), NodeKind::Epic, "2");
    let one = child(two, NodeKind::Epic, "1");
    assert_eq!(kinds(one), [(NodeKind::Issue, "1")]);
    assert_eq!(count(&tree, NodeKind::Epic, "1"), 1);
    assert!(close(1.0, one.metrics.cost_usd));
}

#[test]
fn only_uncharged_sessions_are_unlinked() {
    let t = Temp::new("unlinked");
    let db = build(&t);
    let tree = october(&db).root;
    let keys: Vec<_> = tree
        .children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect();
    assert_eq!(
        keys,
        [
            (NodeKind::Repo, "demo"),
            (NodeKind::Repo, "other"),
            (NodeKind::Unlinked, "")
        ]
    );
    let unlinked = child(&tree, NodeKind::Unlinked, "");
    assert_eq!(unlinked.label, "Unlinked");
    assert!(!unlinked.github);
    // The fifth session was never named.
    assert_eq!(unlinked.children.len(), 1);
    assert_eq!(unlinked.children[0].kind, NodeKind::Agent);
    assert_eq!(unlinked.children[0].metrics.agents, 0, "its root agent");
    assert!(close(SONNET, unlinked.metrics.cost_usd));
    assert_eq!(unlinked.metrics.sessions, 1);

    // other-3 parses, but gh could not fetch `other`: Repo > Issue with no
    // GitHub data (no title, epic, PR or progress), open to its last activity.
    let other = child(&tree, NodeKind::Repo, "other");
    let three = child(other, NodeKind::Issue, "3");
    assert!(!three.github);
    assert_eq!(three.label, "other #3");
    assert!(three.metrics.open && three.metrics.progress.is_none());
    // 16:00:00Z -> 16:00:20Z.
    assert_eq!(three.metrics.elapsed_ms, Some(20 * S));
    labelled(three, NodeKind::Agent, "other-3");
    assert!(close(OPUS, three.metrics.cost_usd));
    // Cached nodes say so.
    assert!(issue(&tree, "9").github);
    assert!(child(issue(&tree, "9"), NodeKind::Pr, "12").github);
}

#[test]
fn with_no_github_cache_issues_sit_under_their_repo() {
    let t = Temp::new("no-gh");
    drop(build_without_gh(&t, "audit-home"));
    let db = Store::open_reader(t.db()).expect("reader");
    let a = october(&db);
    let demo = child(&a.root, NodeKind::Repo, "demo");
    let kinds: Vec<_> = demo
        .children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect();
    // With no cache nothing says #8 is an epic: its session is an issue too.
    assert_eq!(
        kinds,
        [
            (NodeKind::Issue, "8"),
            (NodeKind::Issue, "9"),
            (NodeKind::Issue, "10")
        ]
    );
    let demo9 = child(demo, NodeKind::Issue, "9");
    assert!(!demo9.github);
    assert_eq!(demo9.label, "demo #9");
    // Both root agents and their subagents directly under the issue, no PR
    // level.
    assert_eq!(demo9.children.len(), 4);
    assert!(demo9.children.iter().all(|c| c.kind == NodeKind::Agent));
    // The hand values of the cached build, unchanged.
    let m = &demo9.metrics;
    assert_eq!(
        (m.active_ms, m.wait_ms, m.agent_ms),
        (470 * S, 295 * S, 180 * S)
    );
    assert!(close(0.065, m.cost_usd));
    let tk = &m.tokens;
    assert_eq!(
        (tk.input, tk.output, tk.cache_read, tk.cache_write),
        (7000, 1000, 70_000, 2000)
    );
    assert_eq!((m.sessions, m.agents), (2, 2));
    // Open: Oct 4 14:00:00Z to issue_demo-9's last record, Oct 5 04:05:10Z.
    assert!(m.open && m.progress.is_none());
    assert_eq!(m.elapsed_ms, Some((14 * 3600 + 5 * 60 + 10) * S));
    // Nothing is known closed or merged.
    assert_eq!(
        (a.headline.issues_closed, a.headline.usd_per_merged_pr),
        (0, None)
    );
    check_all_sums(&db);
}

#[test]
fn a_background_subagents_hours_are_its_own_run() {
    let t = Temp::new("background");
    let db = build(&t);
    let tree = october(&db).root;
    let pr = child(issue(&tree, "9"), NodeKind::Pr, "12");
    // issue_demo-9 launches it async (the string flag): [20,140] s.
    let bg = child(pr, NodeKind::Agent, &format!("{A2}/{BG}"));
    assert_eq!(bg.metrics.agent_ms, 120 * S);
    let t = Temp::new("background-no-flag");
    let db = build_nest(&t);
    let tree = october(&db).root;
    let five = child(child(&tree, NodeKind::Repo, "nest"), NodeKind::Issue, "5");
    // nest-5 launches it async with no flag: [6,9] s.
    let bg = child(five, NodeKind::Agent, &format!("{N4}/a7000000000000001"));
    assert_eq!(bg.metrics.agent_ms, 3 * S);
}

#[test]
fn the_date_range_keeps_the_cells_of_its_local_days() {
    let t = Temp::new("dates");
    let db = build(&t);
    let day = |d: &str| audit(&db, &filter(d, d, &[])).expect("audit").root;
    // Local midnight is Oct 5 04:00:00Z, 600 s into issue_demo-9: Oct 4 has
    // [0,600] (active 600 - idle [140,600] = 140 s, waiting [25,140]),
    // Oct 5 has [600,910] (active 910 - 900 = 10 s, idle the rest).
    let oct4 = day("2026-10-04");
    let m = &issue(&oct4, "9").metrics;
    assert_eq!((m.active_ms, m.wait_ms), ((320 + 140) * S, (180 + 115) * S));
    assert_eq!(m.agent_ms, 180 * S);
    // Everything but req_b3 (04:05:10Z, Oct 5 local).
    assert!(close(0.018 + 4.0 * OPUS + 2.0 * SONNET, m.cost_usd));
    assert_eq!(m.sessions, 2);
    let oct5 = day("2026-10-05");
    let m = &issue(&oct5, "9").metrics;
    assert_eq!((m.active_ms, m.wait_ms, m.agent_ms), (10 * S, 0, 0));
    assert!(close(OPUS, m.cost_usd));
    assert_eq!((m.sessions, m.agents), (1, 0));
    // #10 and other-3 are Oct 5 only; the unnamed session Oct 4 only.
    assert!(
        child(child(&oct4, NodeKind::Repo, "demo"), NodeKind::Epic, "8")
            .children
            .iter()
            .all(|c| c.key != "10")
    );
    assert_eq!(child(&oct4, NodeKind::Unlinked, "").children.len(), 1);
    // A range before any work is an empty root.
    let none = day("2026-09-01");
    assert!(none.children.is_empty());
    assert_eq!(none.metrics.sessions, 0);
}

#[test]
fn the_repo_filter_keeps_the_named_repos() {
    let t = Temp::new("repos");
    let db = build(&t);
    let run = |repos: &[&str]| {
        audit(&db, &filter("2026-10-01", "2026-10-31", repos))
            .expect("audit")
            .root
    };
    let demo = run(&["demo"]);
    let kinds: Vec<_> = demo.children.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [NodeKind::Repo]);
    // demo-9 $0.065 + the one opus request each of demo-10 and demo-8.
    assert!(close(0.065 + 2.0 * OPUS, demo.metrics.cost_usd));
    let other = run(&["other"]);
    assert_eq!(other.children.len(), 1);
    let three = child(child(&other, NodeKind::Repo, "other"), NodeKind::Issue, "3");
    assert_eq!(three.children[0].label, "other-3");
    assert!(run(&["nope"]).children.is_empty());
}

#[test]
fn the_repo_filter_takes_the_empty_slug_as_unlinked() {
    let t = Temp::new("unlinked-filter");
    let db = build(&t);
    let run = |repos: &[&str]| {
        audit(&db, &filter("2026-10-01", "2026-10-31", repos))
            .expect("audit")
            .root
    };
    let all = october(&db).root;
    let unlinked = run(&[""]);
    let kinds: Vec<_> = unlinked.children.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [NodeKind::Unlinked]);
    assert_eq!(
        unlinked.children[0],
        *child(&all, NodeKind::Unlinked, ""),
        "the same node as with every repo"
    );
    let kinds: Vec<_> = run(&["other", ""])
        .children
        .iter()
        .map(|c| c.kind)
        .collect();
    assert_eq!(kinds, [NodeKind::Repo, NodeKind::Unlinked]);
}

#[test]
fn the_audit_lists_every_repo_in_range_whatever_the_repo_filter() {
    let t = Temp::new("chips");
    let db = build(&t);
    let run = |from: &str, to: &str, repos: &[&str]| {
        let a = audit(&db, &filter(from, to, repos)).expect("audit");
        (a.repos, a.unlinked)
    };
    let both = (vec!["demo".to_string(), "other".to_string()], true);
    assert_eq!(run("2026-10-01", "2026-10-31", &[]), both);
    assert_eq!(run("2026-10-01", "2026-10-31", &["other"]), both);
    assert_eq!(run("2026-10-01", "2026-10-31", &["nope"]), both);
    // other-3 works on Oct 5 only; the unnamed session on Oct 4 only.
    assert_eq!(
        run("2026-10-05", "2026-10-05", &[]),
        (vec!["demo".to_string(), "other".to_string()], false)
    );
    assert_eq!(
        run("2026-10-04", "2026-10-04", &["demo"]),
        (vec!["demo".to_string()], true)
    );
    assert_eq!(run("2026-09-01", "2026-09-30", &[]), (vec![], false));
}

/// The node at `path` (`[kind, key]` steps) under `node`.
fn at<'a>(node: &'a AuditNode, path: &[(NodeKind, String)]) -> Option<&'a AuditNode> {
    path.iter().try_fold(node, |n, (kind, key)| {
        n.children.iter().find(|c| c.kind == *kind && &c.key == key)
    })
}

/// The keys a node's $ and agent-hours split by (#54).
#[derive(Debug, Clone, Copy)]
enum Split {
    Day,
    Role,
    Model,
}

/// `m`'s split `by`: `(key, $, agent ms)` per slice.
fn split(m: &Metrics, by: Split) -> Vec<(&str, f64, i64)> {
    let slices = match by {
        Split::Day => &m.by_day,
        Split::Role => &m.by_role,
        Split::Model => &m.by_model,
    };
    slices
        .iter()
        .map(|s| (s.key.as_str(), s.cost_usd, s.agent_ms))
        .collect()
}

/// Every node's slices under `by` sum to its own $ and agent-hours, in key
/// order, each key once.
fn check_split_sums(node: &AuditNode, by: Split, path: &str) {
    let path = format!("{path}/{:?}:{}", node.kind, node.key);
    let slices = split(&node.metrics, by);
    let keys: Vec<_> = slices.iter().map(|s| s.0).collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "{path}: {keys:?}");
    let usd: f64 = slices.iter().map(|s| s.1).sum();
    assert!(close(node.metrics.cost_usd, usd), "{path}: $");
    let ms: i64 = slices.iter().map(|s| s.2).sum();
    assert_eq!(node.metrics.agent_ms, ms, "{path}: agent ms");
    for c in &node.children {
        check_split_sums(c, by, &path);
    }
}

/// Every parent's slice under `by` for a key is the sum of its children's
/// slices for that key, and a subagent's node has one role.
fn check_split_children(node: &AuditNode, by: Split, path: &str) {
    let path = format!("{path}/{:?}:{}", node.kind, node.key);
    if is_subagent(node) {
        assert_eq!(node.metrics.by_role.len(), 1, "{path}: roles");
    }
    if node.children.is_empty() {
        return;
    }
    for (key, usd, ms) in split(&node.metrics, by) {
        let kids: Vec<_> = node
            .children
            .iter()
            .flat_map(|c| split(&c.metrics, by))
            .filter(|s| s.0 == key)
            .collect();
        let kid_usd: f64 = kids.iter().map(|s| s.1).sum();
        assert!(close(usd, kid_usd), "{path} {key}: $");
        assert_eq!(
            ms,
            kids.iter().map(|s| s.2).sum::<i64>(),
            "{path} {key}: ms"
        );
    }
    for c in &node.children {
        check_split_children(c, by, &path);
    }
}

/// Each node's slice for `day` is the same node's $ and agent-hours in
/// `one_day`, the tree of that day alone; returns how many slices it checked.
fn check_split_day(
    node: &AuditNode,
    path: &mut Vec<(NodeKind, String)>,
    day: &str,
    one_day: &AuditNode,
) -> usize {
    let mut checked = 0;
    if let Some(&(_, usd, ms)) = split(&node.metrics, Split::Day).iter().find(|s| s.0 == day) {
        let same = at(one_day, path).unwrap_or_else(|| panic!("no {path:?} on {day}"));
        assert!(close(same.metrics.cost_usd, usd), "{path:?} {day}: $");
        assert_eq!(same.metrics.agent_ms, ms, "{path:?} {day}: agent ms");
        checked += 1;
    }
    for c in &node.children {
        path.push((c.kind, c.key.clone()));
        checked += check_split_day(c, path, day, one_day);
        path.pop();
    }
    checked
}

fn nodes(node: &AuditNode) -> usize {
    1 + node.children.iter().map(nodes).sum::<usize>()
}

#[test]
fn teammates_with_no_role_split_by_the_type_they_were_spawned_as() {
    // teams-home: a lead and four teammates, each meta's agentType its name,
    // no name parsing to a role. Each splits under its spawned type
    // (spawnedAgentType, else customAgentType), the one with neither under
    // `teammate` (#86), and the lead, a root agent with no role, under
    // `root`.
    let t = Temp::new("teammates");
    drop(build_without_gh(&t, "teams-home"));
    let db = Store::open_reader(t.db()).expect("reader");
    let root = audit(&db, &filter("2026-10-09", "2026-10-09", &[]))
        .expect("audit")
        .root;
    let types = split(&root.metrics, Split::Role);
    let keys: Vec<_> = types.iter().map(|s| s.0).collect();
    let expected = [
        "general-purpose",
        "implementer",
        "reviewer",
        "root",
        "teammate",
    ];
    assert_eq!(keys, expected);
    let mates: i64 = types.iter().filter(|s| s.0 != "root").map(|s| s.2).sum();
    assert_eq!(mates, root.metrics.agent_ms, "every teammate's wall");
    assert!(types.iter().all(|s| s.1 > 0.0), "each type has its $");
}

#[test]
fn every_node_splits_its_spend_by_day_role_and_model() {
    let t = Temp::new("splits");
    let db = build(&t);
    for (from, to) in [
        ("2026-10-01", "2026-10-31"),
        ("2026-10-04", "2026-10-04"),
        ("2026-10-05", "2026-10-05"),
    ] {
        let tree = audit(&db, &filter(from, to, &[])).expect("audit").root;
        for by in [Split::Day, Split::Role, Split::Model] {
            check_split_sums(&tree, by, &format!("{from}..{to} {by:?}"));
            check_split_children(&tree, by, &format!("{from}..{to} {by:?}"));
        }
        // A day's slice for each node in that day's own tree, and no other.
        for (day, _, _) in split(&tree.metrics, Split::Day) {
            let one_day = audit(&db, &filter(day, day, &[])).expect("audit").root;
            let checked = check_split_day(&tree, &mut Vec::new(), day, &one_day);
            assert_eq!(checked, nodes(&one_day), "{from}..{to} {day}");
        }
    }
    let root = october(&db).root;
    let keys =
        |m: &Metrics, by| -> Vec<String> { split(m, by).iter().map(|s| s.0.to_string()).collect() };
    let m = &root.metrics;
    assert_eq!(keys(m, Split::Day), ["2026-10-04", "2026-10-05"]);
    // No name in audit-home parses to a role: subagents by type, root
    // agents `root`.
    assert_eq!(keys(m, Split::Role), ["implementer", "recon", "root"]);
    assert_eq!(
        keys(m, Split::Model),
        ["claude-opus-5-5", "claude-sonnet-5-5"]
    );
    assert_eq!(m.by_role[1].agent_ms, 120 * S, "recon");
    assert_eq!(m.by_role[2].agent_ms, 0, "root agents have no agent-hours");
    // The closed issue's PR: demo-9 on Oct 4, its -s2 across midnight.
    let pr = child(issue(&root, "9"), NodeKind::Pr, "12");
    assert_eq!(keys(&pr.metrics, Split::Day), ["2026-10-04", "2026-10-05"]);
    let usd: f64 = pr.metrics.by_day.iter().map(|s| s.cost_usd).sum();
    assert!(close(0.065, usd));
}

/// Checks every node with children against the sum of its children.
fn check_sums(node: &AuditNode, path: &str) {
    let path = format!("{path}/{:?}:{}", node.kind, node.key);
    if node.children.is_empty() {
        return;
    }
    let m = &node.metrics;
    let kids = || node.children.iter().map(|c| &c.metrics);
    let sum = |f: fn(&strate_core::store::views::Metrics) -> i64| kids().map(f).sum::<i64>();
    assert!(
        close(m.cost_usd, kids().map(|k| k.cost_usd).sum()),
        "{path}: $"
    );
    assert_eq!(m.unpriced, sum(|k| k.unpriced), "{path}: unpriced");
    assert_eq!(m.tokens.input, sum(|k| k.tokens.input), "{path}: input");
    assert_eq!(m.tokens.output, sum(|k| k.tokens.output), "{path}: output");
    assert_eq!(
        m.tokens.cache_read,
        sum(|k| k.tokens.cache_read),
        "{path}: read"
    );
    assert_eq!(
        m.tokens.cache_write,
        sum(|k| k.tokens.cache_write),
        "{path}: write"
    );
    assert_eq!(m.agent_ms, sum(|k| k.agent_ms), "{path}: agent ms");
    assert_eq!(m.sessions, sum(|k| k.sessions), "{path}: sessions");
    assert_eq!(m.agents, sum(|k| k.agents), "{path}: agents");
    // Subagents run in parallel with their root agent: a node's active and
    // waiting are its root agents', not its subagents' too.
    let line = || node.children.iter().filter(|c| !is_subagent(c));
    let active: i64 = line().map(|c| c.metrics.active_ms).sum();
    let wait: i64 = line().map(|c| c.metrics.wait_ms).sum();
    assert_eq!(m.active_ms, active, "{path}: active");
    assert_eq!(m.wait_ms, wait, "{path}: waiting");
    for c in &node.children {
        check_sums(c, &path);
    }
}

/// The cells in `[from, to]` that are not idle only, summed: $, the 4 token counts, agent ms
/// (subagent wall), active and waiting (orchestrator cells), sessions (each
/// session once per effective charge of its agents) and subagents.
fn cell_totals(db: &Connection, from: &str, to: &str) -> (f64, [i64; 4], [i64; 3], [i64; 2]) {
    db.query_row(
        "SELECT ifnull(sum(c.cost_usd), 0), sum(c.input_tokens), sum(c.output_tokens),
             sum(c.cache_read_tokens), sum(c.cache_write_tokens),
             sum(CASE WHEN c.agent_id IS NOT NULL THEN c.wall_ms ELSE 0 END),
             sum(CASE WHEN c.agent_id IS NULL THEN c.active_ms ELSE 0 END),
             sum(CASE WHEN c.agent_id IS NULL THEN c.wait_ms ELSE 0 END),
             count(DISTINCT c.session_id || '/' || ifnull(x.charge, '')),
             count(DISTINCT c.session_id || '/' || c.agent_id)
         FROM audit_cells AS c
         LEFT JOIN agents AS a ON a.session_id = c.session_id AND a.agent_id IS c.agent_id
         LEFT JOIN agent_charges AS x ON x.agent = a.id
         WHERE c.day BETWEEN ?1 AND ?2
             AND NOT (c.active_ms = 0 AND c.wait_ms = 0 AND c.cost_usd IS NULL
                 AND c.unpriced = 0
                 AND c.input_tokens + c.output_tokens + c.cache_read_tokens
                     + c.cache_write_tokens = 0
                 AND (c.agent_id IS NULL OR c.wall_ms = 0))",
        [from, to],
        |r| {
            Ok((
                r.get(0)?,
                [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                [r.get(5)?, r.get(6)?, r.get(7)?],
                [r.get(8)?, r.get(9)?],
            ))
        },
    )
    .expect("cells")
}

#[test]
fn every_parent_is_the_sum_of_its_children() {
    let t = Temp::new("property");
    check_all_sums(&build(&t));
}

/// The property over 3 ranges.
fn check_all_sums(db: &Connection) {
    for (from, to) in [
        ("2026-10-01", "2026-10-31"),
        ("2026-10-04", "2026-10-04"),
        ("2026-10-05", "2026-10-05"),
    ] {
        let (usd, tokens, [agent_ms, active, wait], [sessions, agents]) = cell_totals(db, from, to);
        assert!(sessions > 0, "the range has work");
        let tree = audit(db, &filter(from, to, &[])).expect("audit").root;
        check_sums(&tree, &format!("{from}..{to}"));
        let m = &tree.metrics;
        assert!(close(usd, m.cost_usd), "{from}..{to} root $");
        let t = &m.tokens;
        assert_eq!(tokens, [t.input, t.output, t.cache_read, t.cache_write]);
        assert_eq!(
            [agent_ms, active, wait],
            [m.agent_ms, m.active_ms, m.wait_ms]
        );
        assert_eq!([sessions, agents], [m.sessions, m.agents]);
    }
}

#[test]
fn the_headline_counts_closed_issues_and_merged_prs() {
    let t = Temp::new("headline");
    let db = build(&t);
    let h = october(&db).headline;
    assert_eq!(h.issues_closed, 1);
    assert_eq!(h.median_elapsed_ms, Some(22 * 3600 * S));
    assert!(close(0.065, h.usd_per_closed_issue.expect("closed issue")));
    assert!(close(0.065, h.usd_per_merged_pr.expect("merged pr")));
    assert!(h.cloud_uncounted, "#29");
    // Oct 4 only: the same issue, with its Oct 4 $.
    let oct4 = audit(&db, &filter("2026-10-04", "2026-10-04", &[]))
        .expect("audit")
        .headline;
    assert!(close(0.057, oct4.usd_per_closed_issue.expect("closed")));
    let none = audit(&db, &filter("2026-10-01", "2026-10-31", &["other"]))
        .expect("audit")
        .headline;
    assert_eq!((none.issues_closed, none.median_elapsed_ms), (0, None));
    assert_eq!(
        (none.usd_per_closed_issue, none.usd_per_merged_pr),
        (None, None)
    );
}

#[test]
fn a_bad_day_is_an_error_not_an_empty_tree() {
    let t = Temp::new("bad-day");
    let db = build(&t);
    assert!(audit(&db, &filter("2026-10", "2026-10-31", &[])).is_err());
    assert!(audit(&db, &filter("2026-10-01", "lorem", &[])).is_err());
}

#[test]
fn the_filter_and_tree_use_the_webview_shape() {
    let f: AuditFilter = serde_json::from_value(serde_json::json!({
        "from": "2026-10-01", "to": "2026-10-31", "repos": ["demo"]
    }))
    .expect("filter");
    assert_eq!(f, filter("2026-10-01", "2026-10-31", &["demo"]));
    let t = Temp::new("shape");
    let db = build(&t);
    let value = serde_json::to_value(october(&db)).expect("json");
    let mut keys: Vec<_> = value.as_object().expect("audit").keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["githubErrors", "headline", "repos", "root", "unlinked"]
    );
    assert_eq!(
        value["githubErrors"][0]
            .as_object()
            .map(|e| e.keys().cloned().collect::<Vec<_>>()),
        Some(vec!["error".to_string(), "repo".to_string()])
    );
    let mut keys: Vec<_> = value["headline"]
        .as_object()
        .expect("headline")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "cloudUncounted",
            "issuesClosed",
            "medianElapsedMs",
            "usdPerClosedIssue",
            "usdPerMergedPr"
        ]
    );
    let root = &value["root"];
    assert_eq!(root["kind"], "root");
    let mut keys: Vec<_> = root["metrics"]
        .as_object()
        .expect("metrics")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "activeMs",
            "agentMs",
            "agents",
            "byDay",
            "byModel",
            "byRole",
            "costUsd",
            "elapsedMs",
            "open",
            "progress",
            "sessions",
            "tokens",
            "unpriced",
            "waitMs"
        ]
    );
    let mut keys: Vec<_> = root["metrics"]["tokens"]
        .as_object()
        .expect("tokens")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(keys, ["cacheRead", "cacheWrite", "input", "output"]);
    let mut keys: Vec<_> = root["metrics"]["byDay"][0]
        .as_object()
        .expect("slice")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(keys, ["agentMs", "costUsd", "key"]);
    let mut keys: Vec<_> = root.as_object().expect("node").keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["children", "github", "key", "kind", "label", "metrics"]
    );
    assert_eq!(root["children"][2]["kind"], "unlinked");
    assert_eq!(serde_json::json!(NodeKind::Topic), "topic");
    // The Session level is gone (#110).
    assert!(serde_json::from_value::<NodeKind>(serde_json::json!("session")).is_err());
    assert_eq!(
        serde_json::from_value::<NodeKind>(serde_json::json!("agent")).expect("kind"),
        NodeKind::Agent
    );
}

#[test]
fn the_headline_elapsed_is_the_whole_lifetime_whatever_the_range() {
    let t = Temp::new("lifetime");
    let db = build(&t);
    // Oct 5 holds only issue_demo-9's tail, but #9's elapsed still runs from
    // its first session ever (Oct 4 14:00:00Z) to the close (Oct 5 12:00:00Z).
    let oct5 = audit(&db, &filter("2026-10-05", "2026-10-05", &[]))
        .expect("audit")
        .headline;
    assert_eq!(oct5.issues_closed, 1);
    assert_eq!(oct5.median_elapsed_ms, Some(22 * 3600 * S));
    assert!(close(OPUS, oct5.usd_per_closed_issue.expect("closed")));
    // A range with no work on #9 does not count it.
    let none = audit(&db, &filter("2026-10-06", "2026-10-31", &[]))
        .expect("audit")
        .headline;
    assert_eq!((none.issues_closed, none.median_elapsed_ms), (0, None));
}

#[test]
fn the_audit_lists_the_repos_github_could_not_fetch() {
    let t = Temp::new("gh-errors");
    let db = build(&t);
    let errors = october(&db).github_errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].repo, "other");
    assert!(errors[0].error.contains("Could not resolve"), "{errors:?}");
    let t = Temp::new("gh-errors-none");
    drop(build_without_gh(&t, "audit-home"));
    let db = Store::open_reader(t.db()).expect("reader");
    assert!(october(&db).github_errors.is_empty());
}

/// A store holding exactly `sql`'s rows (no transcripts): the store is
/// created, then the rows go in on a plain connection, then a reader.
fn synthetic(t: &Temp, sql: &str) -> Connection {
    drop(Store::open_in(t.db(), "UTC").expect("open"));
    Connection::open(t.db())
        .expect("raw")
        .execute_batch(sql)
        .expect("rows");
    Store::open_reader(t.db()).expect("reader")
}

/// Repo `lab`: #1 and #2 closed as completed, both closed by PR #5 of
/// `lab-ui` (merged), with $1 and $3 of work; #3 closed as not planned,
/// with $2; #4 not cached, one session Friday to Monday, idle on the
/// weekend. Each session's orchestrator is charged its issue.
const LAB: &str = "
    INSERT INTO sessions (session_id, path, started_at) VALUES
        ('s1', '/p/s1.jsonl', '2026-10-02T10:00:00.000Z'),
        ('s2', '/p/s2.jsonl', '2026-10-02T12:00:00.000Z'),
        ('s3', '/p/s3.jsonl', '2026-10-02T14:00:00.000Z'),
        ('s4', '/p/s4.jsonl', '2026-10-09T20:00:00.000Z');
    INSERT INTO charges (id, key, kind, repo, number) VALUES
        (1, 'issue_lab-1', 'issue', 'lab', 1), (2, 'issue_lab-2', 'issue', 'lab', 2),
        (3, 'issue_lab-3', 'issue', 'lab', 3), (4, 'issue_lab-4', 'issue', 'lab', 4);
    INSERT INTO agents (session_id, kind, own_charge, own_charge_source) VALUES
        ('s1', 'orchestrator', 1, 'name'), ('s2', 'orchestrator', 2, 'name'),
        ('s3', 'orchestrator', 3, 'name'), ('s4', 'orchestrator', 4, 'name');
    INSERT INTO audit_cells (session_id, agent_id, day, model, wall_ms,
        active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens) VALUES
        ('s1', NULL, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('s2', NULL, '2026-10-02', 'm', 1000, 1000, 0, 3.0, 0, 10, 1, 0, 0),
        ('s3', NULL, '2026-10-02', 'm', 1000, 1000, 0, 2.0, 0, 10, 1, 0, 0),
        ('s4', NULL, '2026-10-09', 'm', 3600000, 600000, 0, 0.5, 0, 10, 1, 0, 0),
        ('s4', NULL, '2026-10-10', NULL, 86400000, 0, 0, NULL, 0, 0, 0, 0, 0),
        ('s4', NULL, '2026-10-11', NULL, 86400000, 0, 0, NULL, 0, 0, 0, 0, 0),
        ('s4', NULL, '2026-10-12', 'm', 3600000, 600000, 0, 0.5, 0, 10, 1, 0, 0);
    INSERT INTO gh_repos (slug, owner, name, fetched_at) VALUES ('lab', 'example', 'lab', 1);
    INSERT INTO gh_issues (repo, number, title, state, state_reason, created_at, closed_at,
        parent, sub_total, sub_completed) VALUES
        ('lab', 1, 'Lorem', 'CLOSED', 'COMPLETED', '2026-10-01T00:00:00Z', '2026-10-03T10:00:00Z', NULL, 0, 0),
        ('lab', 2, 'Ipsum', 'CLOSED', 'COMPLETED', '2026-10-01T00:00:00Z', '2026-10-03T12:00:00Z', NULL, 0, 0),
        ('lab', 3, 'Dolor', 'CLOSED', 'NOT_PLANNED', '2026-10-01T00:00:00Z', '2026-10-03T14:00:00Z', NULL, 0, 0);
    INSERT INTO gh_prs (repo, issue, number, state, created_at, merged_at, url) VALUES
        ('lab', 1, 5, 'MERGED', '2026-10-02T00:00:00Z', '2026-10-03T09:00:00Z', 'https://github.com/example/lab-ui/pull/5'),
        ('lab', 2, 5, 'MERGED', '2026-10-02T00:00:00Z', '2026-10-03T09:00:00Z', 'https://github.com/example/lab-ui/pull/5');
";

fn lab(db: &Connection, from: &str, to: &str) -> Audit {
    audit(db, &filter(from, to, &[])).expect("audit")
}

#[test]
fn a_pr_closing_two_issues_counts_once_in_dollars_per_merged_pr() {
    let t = Temp::new("pr-twice");
    let db = synthetic(&t, LAB);
    let h = lab(&db, "2026-10-01", "2026-10-31").headline;
    // One PR, $1 + $3 of work under it: not two PRs at $1 and $3.
    assert!(close(4.0, h.usd_per_merged_pr.expect("merged")), "{h:?}");
}

#[test]
fn only_issues_closed_as_completed_count_in_the_headline() {
    let t = Temp::new("completed");
    let db = synthetic(&t, LAB);
    let a = lab(&db, "2026-10-01", "2026-10-31");
    let h = &a.headline;
    assert_eq!(h.issues_closed, 2, "#3 was closed as not planned");
    assert!(close(2.0, h.usd_per_closed_issue.expect("closed")), "{h:?}");
    // #1: Oct 2 10:00 -> Oct 3 10:00, #2: Oct 2 12:00 -> Oct 3 12:00.
    assert_eq!(h.median_elapsed_ms, Some(24 * 3600 * S));
    // #3 keeps its $ in the tree.
    let three = child(child(&a.root, NodeKind::Repo, "lab"), NodeKind::Issue, "3");
    assert!(close(2.0, three.metrics.cost_usd));
    assert!(!three.metrics.open);
}

#[test]
fn idle_only_days_show_nothing() {
    let t = Temp::new("idle");
    let db = synthetic(&t, LAB);
    let sat = lab(&db, "2026-10-10", "2026-10-10").root;
    assert!(sat.children.is_empty(), "{:?}", sat.children);
    assert_eq!(sat.metrics.sessions, 0);
    let weekend = lab(&db, "2026-10-10", "2026-10-11").root;
    assert!(weekend.children.is_empty());
    assert!(weekend.metrics.by_day.is_empty());
    // Friday to Monday: the session, counted once, with its two working days.
    let four = lab(&db, "2026-10-09", "2026-10-12").root;
    let days: Vec<_> = four.metrics.by_day.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(days, ["2026-10-09", "2026-10-12"]);
    assert_eq!(four.metrics.sessions, 1);
    assert!(close(1.0, four.metrics.cost_usd));
}

/// One orchestrator per charge kind, each its own session with one cell on
/// Oct 2 (session sN costs $N): s1 epic lab#1, s2 repo lab, s3 PR lab#5
/// (cached: it closes #2, a sub-issue of epic #1), s4 PR lab#9 (not
/// cached), s5 topic notes, s6 issue lab#2 (its only PR is #5), s7 none.
const KINDS: &str = "
    INSERT INTO sessions (session_id, path, started_at) VALUES
        ('s1', '/p/s1.jsonl', '2026-10-02T10:00:00.000Z'),
        ('s2', '/p/s2.jsonl', '2026-10-02T11:00:00.000Z'),
        ('s3', '/p/s3.jsonl', '2026-10-02T12:00:00.000Z'),
        ('s4', '/p/s4.jsonl', '2026-10-02T13:00:00.000Z'),
        ('s5', '/p/s5.jsonl', '2026-10-02T14:00:00.000Z'),
        ('s6', '/p/s6.jsonl', '2026-10-02T15:00:00.000Z'),
        ('s7', '/p/s7.jsonl', '2026-10-02T16:00:00.000Z');
    INSERT INTO charges (id, key, kind, repo, number, topic) VALUES
        (1, 'epic_lab-1', 'epic', 'lab', 1, NULL),
        (2, 'repo_lab', 'repo', 'lab', NULL, NULL),
        (3, 'pr_lab-5', 'pr', 'lab', 5, NULL),
        (4, 'pr_lab-9', 'pr', 'lab', 9, NULL),
        (5, 'topic_notes', 'topic', NULL, NULL, 'notes'),
        (6, 'issue_lab-2', 'issue', 'lab', 2, NULL);
    INSERT INTO agents (session_id, kind, own_charge, own_charge_source) VALUES
        ('s1', 'orchestrator', 1, 'name'), ('s2', 'orchestrator', 2, 'name'),
        ('s3', 'orchestrator', 3, 'name'), ('s4', 'orchestrator', 4, 'name'),
        ('s5', 'orchestrator', 5, 'name'), ('s6', 'orchestrator', 6, 'name'),
        ('s7', 'orchestrator', NULL, NULL);
    INSERT INTO audit_cells (session_id, agent_id, day, model, wall_ms, active_ms, wait_ms,
        cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
        cache_write_tokens) VALUES
        ('s1', NULL, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('s2', NULL, '2026-10-02', 'm', 1000, 1000, 0, 2.0, 0, 10, 1, 0, 0),
        ('s3', NULL, '2026-10-02', 'm', 1000, 1000, 0, 3.0, 0, 10, 1, 0, 0),
        ('s4', NULL, '2026-10-02', 'm', 1000, 1000, 0, 4.0, 0, 10, 1, 0, 0),
        ('s5', NULL, '2026-10-02', 'm', 1000, 1000, 0, 5.0, 0, 10, 1, 0, 0),
        ('s6', NULL, '2026-10-02', 'm', 1000, 1000, 0, 6.0, 0, 10, 1, 0, 0),
        ('s7', NULL, '2026-10-02', 'm', 1000, 1000, 0, 7.0, 0, 10, 1, 0, 0);
    INSERT INTO gh_repos (slug, owner, name, fetched_at) VALUES ('lab', 'example', 'lab', 1);
    INSERT INTO gh_issues (repo, number, title, state, created_at, parent, sub_total,
        sub_completed) VALUES
        ('lab', 1, 'Lorem', 'OPEN', '2026-10-01T00:00:00Z', NULL, 1, 0),
        ('lab', 2, 'Ipsum', 'OPEN', '2026-10-01T00:00:00Z', 1, 0, 0);
    INSERT INTO gh_prs (repo, issue, number, state, created_at, merged_at, url) VALUES
        ('lab', 2, 5, 'OPEN', '2026-10-02T00:00:00Z', NULL, 'https://github.com/example/lab/pull/5');
";

#[test]
fn each_charge_kind_places_its_root_agent_on_its_own_path() {
    use NodeKind::{Agent, Epic, Issue, Pr, Repo, Topic, Unlinked};
    let t = Temp::new("kinds");
    let db = synthetic(&t, KINDS);
    let a = lab(&db, "2026-10-01", "2026-10-31");
    let root = &a.root;
    assert_eq!(
        kinds(root),
        [(Repo, "lab"), (Topic, "notes"), (Unlinked, "")]
    );
    // A repo charge sits right under its repo, and so does a PR the cache
    // doesn't hold.
    let repo = child(root, Repo, "lab");
    assert_eq!(
        kinds(repo),
        [(Epic, "1"), (Pr, "9"), (Agent, "s2/repo_lab")]
    );
    assert!(close(1.0 + 2.0 + 3.0 + 4.0 + 6.0, repo.metrics.cost_usd));
    // An epic charge sits right under its epic, beside its sub-issues.
    let epic = child(repo, Epic, "1");
    assert_eq!(epic.label, "#1 Lorem");
    assert_eq!(kinds(epic), [(Issue, "2"), (Agent, "s1/epic_lab-1")]);
    assert!(close(1.0 + 3.0 + 6.0, epic.metrics.cost_usd));
    // A cached PR charge sits under the issue it closes, with that issue's
    // own work (its only PR), each its own root agent.
    let pr5 = child(child(epic, Issue, "2"), Pr, "5");
    assert!(pr5.github);
    assert_eq!(
        kinds(pr5),
        [(Agent, "s3/pr_lab-5"), (Agent, "s6/issue_lab-2")]
    );
    assert!(close(3.0 + 6.0, pr5.metrics.cost_usd));
    let pr9 = child(repo, Pr, "9");
    assert_eq!((pr9.label.as_str(), pr9.github), ("PR #9", false));
    assert!(pr9.metrics.open);
    assert_eq!(kinds(pr9), [(Agent, "s4/pr_lab-9")]);
    // A topic is a top-level node of its own.
    let topic = child(root, Topic, "notes");
    assert_eq!((topic.label.as_str(), topic.github), ("notes", false));
    assert_eq!(kinds(topic), [(Agent, "s5/topic_notes")]);
    assert!(close(5.0, topic.metrics.cost_usd));
    let lead = child(topic, Agent, "s5/topic_notes");
    assert_eq!(lead.label, "s5", "no name: the session id");
    assert!(lead.children.is_empty(), "an Agent node is a leaf");
    let unlinked = child(root, Unlinked, "");
    assert_eq!(kinds(unlinked), [(Agent, "s7/")]);
    assert!(close(7.0, unlinked.metrics.cost_usd));
    assert!(close(28.0, root.metrics.cost_usd));
    assert_eq!(root.metrics.sessions, 7);
    check_sums(root, "kinds");
    assert_eq!(
        (a.repos.clone(), a.unlinked),
        (vec!["lab".to_string()], true)
    );
}

/// Issue lab#1, not cached. Chain s1: s1 (10:00 to 10:30, named `lead`,
/// role manager) and its /clear successor s2 (12:00 to 12:10, no name or
/// role); s3 (11:00 to 11:05, no name or role) is its own chain. s1's
/// subagents, each charged #1: a1 (role impl, type implementer), a2 (type
/// recon) and a3 (neither), running from 10:10, 10:20 and 10:05. s3's a4
/// has a cell but no `agents` row, so no charge. Cell $: s1 1, a1 2, a2 3,
/// a3 4, s2 5, s3 6, a4 7.
const ROLES: &str = "
    INSERT INTO sessions (session_id, path, started_at, process_root) VALUES
        ('s1', '/p/s1.jsonl', '2026-10-02T10:00:00.000Z', 's1'),
        ('s2', '/p/s2.jsonl', '2026-10-02T12:00:00.000Z', 's1'),
        ('s3', '/p/s3.jsonl', '2026-10-02T11:00:00.000Z', 's3');
    INSERT INTO charges (id, key, kind, repo, number) VALUES
        (1, 'issue_lab-1', 'issue', 'lab', 1);
    INSERT INTO agents (session_id, agent_id, kind, name, role, agent_type, own_charge,
        own_charge_source) VALUES
        ('s1', NULL, 'orchestrator', 'lead', 'manager', NULL, 1, 'name'),
        ('s1', 'a1', 'subagent', NULL, 'impl', 'implementer', 1, 'name'),
        ('s1', 'a2', 'subagent', NULL, NULL, 'recon', 1, 'name'),
        ('s1', 'a3', 'subagent', NULL, NULL, NULL, 1, 'name'),
        ('s2', NULL, 'orchestrator', NULL, NULL, NULL, 1, 'name'),
        ('s3', NULL, 'orchestrator', NULL, NULL, NULL, 1, 'name');
    INSERT INTO events (session_id, agent_id, file, byte_offset, raw, timestamp, type) VALUES
        ('s1', NULL, '/p/s1.jsonl', 1, '{}', '2026-10-02T10:30:00.000Z', 'assistant'),
        ('s1', 'a1', '/p/a1.jsonl', 1, '{}', '2026-10-02T10:10:00.000Z', 'user'),
        ('s1', 'a1', '/p/a1.jsonl', 2, '{}', '2026-10-02T10:11:00.000Z', 'assistant'),
        ('s1', 'a2', '/p/a2.jsonl', 1, '{}', '2026-10-02T10:20:00.000Z', 'user'),
        ('s1', 'a2', '/p/a2.jsonl', 2, '{}', '2026-10-02T10:22:00.000Z', 'assistant'),
        ('s1', 'a3', '/p/a3.jsonl', 1, '{}', '2026-10-02T10:05:00.000Z', 'user'),
        ('s1', 'a3', '/p/a3.jsonl', 2, '{}', '2026-10-02T10:08:00.000Z', 'assistant'),
        ('s2', NULL, '/p/s2.jsonl', 1, '{}', '2026-10-02T12:10:00.000Z', 'assistant'),
        ('s3', NULL, '/p/s3.jsonl', 1, '{}', '2026-10-02T11:05:00.000Z', 'assistant');
    INSERT INTO audit_cells (session_id, agent_id, day, model, wall_ms, active_ms, wait_ms,
        cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
        cache_write_tokens) VALUES
        ('s1', NULL, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('s1', 'a1', '2026-10-02', 'm', 60000, 60000, 0, 2.0, 0, 10, 1, 0, 0),
        ('s1', 'a2', '2026-10-02', 'm', 120000, 120000, 0, 3.0, 0, 10, 1, 0, 0),
        ('s1', 'a3', '2026-10-02', 'm', 180000, 180000, 0, 4.0, 0, 10, 1, 0, 0),
        ('s2', NULL, '2026-10-02', 'm', 1000, 1000, 0, 5.0, 0, 10, 1, 0, 0),
        ('s3', NULL, '2026-10-02', 'm', 1000, 1000, 0, 6.0, 0, 10, 1, 0, 0),
        ('s3', 'a4', '2026-10-02', 'm', 1000, 1000, 0, 7.0, 0, 10, 1, 0, 0);
";

#[test]
fn each_root_agent_leads_its_chains_subagents_and_every_cell_splits_by_role() {
    use NodeKind::{Agent, Issue, Repo, Unlinked};
    let t = Temp::new("roles");
    let db = synthetic(&t, ROLES);
    let root = lab(&db, "2026-10-01", "2026-10-31").root;
    let one = child(child(&root, Repo, "lab"), Issue, "1");
    // The s1 chain's root agent (both links), then its subagents by run
    // start, not id; then s3's chain, which starts after s1's.
    assert_eq!(
        kinds(one),
        [
            (Agent, "s1/issue_lab-1"),
            (Agent, "s1/a3"),
            (Agent, "s1/a1"),
            (Agent, "s1/a2"),
            (Agent, "s3/issue_lab-1")
        ]
    );
    let chain = child(one, Agent, "s1/issue_lab-1");
    // Labelled by its earliest session's name; its sessions' 30 + 10 min.
    assert_eq!(chain.label, "lead");
    assert_eq!(chain.metrics.elapsed_ms, Some(40 * 60 * S));
    assert_eq!((chain.metrics.sessions, chain.metrics.agents), (2, 0));
    assert!(close(1.0 + 5.0, chain.metrics.cost_usd));
    assert_eq!(
        (chain.metrics.active_ms, chain.metrics.agent_ms),
        (2 * S, 0)
    );
    let lone = child(one, Agent, "s3/issue_lab-1");
    assert_eq!(lone.label, "s3", "no name: the session id");
    assert_eq!(lone.metrics.elapsed_ms, Some(5 * 60 * S));
    assert_eq!(child(one, Agent, "s1/a1").metrics.agents, 1);
    // Each cell's role: the agent's own (manager, impl), else a subagent's
    // type (recon), else `root` for a root agent, else unknown.
    let roles = |m: &Metrics| -> Vec<(String, f64)> {
        m.by_role
            .iter()
            .map(|s| (s.key.clone(), s.cost_usd))
            .collect()
    };
    let slice = |key: &str, usd: f64| (key.to_string(), usd);
    assert_eq!(
        roles(&one.metrics),
        [
            slice("", 4.0),
            slice("impl", 2.0),
            slice("manager", 1.0),
            slice("recon", 3.0),
            slice("root", 5.0 + 6.0)
        ]
    );
    assert_eq!(
        roles(&chain.metrics),
        [slice("manager", 1.0), slice("root", 5.0)]
    );
    // A subagent with no `agents` row: no charge, no role, its id.
    let unlinked = child(&root, Unlinked, "");
    assert_eq!(kinds(unlinked), [(Agent, "s3/a4")]);
    let a4 = child(unlinked, Agent, "s3/a4");
    assert_eq!(a4.label, "a4");
    assert_eq!(roles(&a4.metrics), [slice("", 7.0)]);
    // Cost parity: the root is every cell once.
    assert!(close(28.0, root.metrics.cost_usd));
    check_sums(&root, "roles");
    for kind in [Split::Day, Split::Role, Split::Model] {
        check_split_sums(&root, kind, "roles");
        check_split_children(&root, kind, "roles");
    }
}

#[test]
fn the_repo_filter_reads_each_sessions_charge_repo() {
    let t = Temp::new("kinds-filter");
    let db = synthetic(&t, KINDS);
    let run = |repos: &[&str]| {
        audit(&db, &filter("2026-10-01", "2026-10-31", repos))
            .expect("audit")
            .root
    };
    let lab_only = run(&["lab"]);
    assert_eq!(kinds(&lab_only), [(NodeKind::Repo, "lab")]);
    assert!(close(16.0, lab_only.metrics.cost_usd));
    // A topic and no charge are in no repo: the empty slug.
    let none = run(&[""]);
    assert_eq!(
        kinds(&none),
        [(NodeKind::Topic, "notes"), (NodeKind::Unlinked, "")]
    );
    assert!(close(12.0, none.metrics.cost_usd));
    assert!(close(28.0, run(&["lab", ""]).metrics.cost_usd));
}

const M1: &str = "5e55f1f1-0000-4000-8000-0000000000d1";
const M2: &str = "5e55f1f1-0000-4000-8000-0000000000d2";
const M3: &str = "5e55f1f1-0000-4000-8000-0000000000d3";
const M4: &str = "5e55f1f1-0000-4000-8000-0000000000d4";
const M5: &str = "5e55f1f1-0000-4000-8000-0000000000d5";
const M6: &str = "5e55f1f1-0000-4000-8000-0000000000d6";
const M7: &str = "5e55f1f1-0000-4000-8000-0000000000d7";
const MA1: &str = "a8000000000000001";
const MA2: &str = "a8000000000000002";
const MA3: &str = "a8000000000000003";
const MB1: &str = "a8000000000000004";

/// Cold build of `tests/fixtures/model-home` in Toronto, its GitHub cache
/// (`strate`) and the registry row naming the background process M7.
fn build_model(t: &Temp) -> Connection {
    let mut store = build_without_gh(t, "model-home");
    // Its issues' parents are example/strate's, so fetched as that owner's.
    let strate =
        fetch_repo(&mut scripted(&["model-home.json"]), "example", "strate").expect("fetch");
    store
        .gh_replace("strate", "example", &strate, 1)
        .expect("gh");
    let background = RegistryEvent::Appeared {
        entry: RegistryEntry {
            cwd: "/work/bg".into(),
            kind: Kind::Background,
            started_at: 1,
            pid: None,
            id: Some("bg-lorem".into()),
            session_id: Some(M7.into()),
            name: Some("impl_issue_strate-111".into()),
            status: None,
            waiting_for: None,
            state: None,
        },
        at_ms: 1_000,
    };
    store.apply_registry(&[background]).expect("registry");
    drop(store);
    Store::open_reader(t.db()).expect("reader")
}

#[test]
fn model_home_files_every_agent_under_its_own_charge() {
    use NodeKind::{Agent, Epic, Issue, Pr, Repo, Topic, Unlinked};
    let t = Temp::new("model-home");
    let db = build_model(&t);
    let day = filter("2026-10-20", "2026-10-20", &[]);
    let root = audit(&db, &day).expect("audit").root;
    assert_eq!(
        kinds(&root),
        [(Repo, "strate"), (Topic, "hub"), (Unlinked, "")]
    );
    let strate = child(&root, Repo, "strate");
    assert_eq!(
        kinds(strate),
        [(Epic, "105"), (Issue, "110"), (Issue, "111")]
    );
    // M1 works #107 by its branch (PR #120's head), with a1, a2 and M2,
    // which a1 messaged first. Its reviewer a3 is charged PR #120 by name:
    // it sits under that PR (closing #107, the lowest of its issues) after
    // M1's own subagents, its chain's root agent having no work in that
    // charge. M2's chain starts later, so its root agent comes last.
    let pr = child(child(child(strate, Epic, "105"), Issue, "107"), Pr, "120");
    let (m1, m2) = (
        format!("{M1}/issue_strate-107"),
        format!("{M2}/issue_strate-107"),
    );
    let (a1, a2, a3) = (
        format!("{M1}/{MA1}"),
        format!("{M1}/{MA2}"),
        format!("{M1}/{MA3}"),
    );
    assert_eq!(
        kinds(pr),
        [
            (Agent, m1.as_str()),
            (Agent, a1.as_str()),
            (Agent, a2.as_str()),
            (Agent, a3.as_str()),
            (Agent, m2.as_str())
        ]
    );
    let own = child(pr, Agent, &m1);
    assert_eq!(own.label, "manager_epic_strate-105");
    // Opus $0.008 a request: M1's two dispatches, a1's two; a2 sonnet.
    assert!(close(2.0 * OPUS, own.metrics.cost_usd));
    assert!(close(2.0 * OPUS, child(pr, Agent, &a1).metrics.cost_usd));
    assert!(close(SONNET, child(pr, Agent, &a2).metrics.cost_usd));
    let review = child(pr, Agent, &a3);
    assert!(close(SONNET, review.metrics.cost_usd));
    // a3's cell is the first of M1 in PR #120's charge.
    assert_eq!((review.metrics.sessions, review.metrics.agents), (1, 1));
    // Its elapsed is a3's run, 10:02:10 to 10:02:20, not M1's session.
    assert_eq!(review.metrics.elapsed_ms, Some(10 * S));
    assert!(close(OPUS, child(pr, Agent, &m2).metrics.cost_usd));
    // M3 by name; the background chain M7 -> M8 by its registry name, one
    // root agent, an issue the cache doesn't hold.
    let m3 = child(
        child(strate, Issue, "110"),
        Agent,
        &format!("{M3}/issue_strate-110"),
    );
    assert!(close(OPUS, m3.metrics.cost_usd));
    let bg = child(strate, Issue, "111");
    assert_eq!((bg.label.as_str(), bg.github), ("strate #111", false));
    let chain = format!("{M7}/issue_strate-111");
    assert_eq!(kinds(bg), [(Agent, chain.as_str())]);
    let chain = child(bg, Agent, &chain);
    assert_eq!(chain.metrics.sessions, 2);
    assert!(close(2.0 * OPUS, chain.metrics.cost_usd));
    // director-hub's topic: M4 with b1, and M5, which b1 messaged.
    let hub = child(&root, Topic, "hub");
    let (m4, b1, m5) = (
        format!("{M4}/topic_hub"),
        format!("{M4}/{MB1}"),
        format!("{M5}/topic_hub"),
    );
    assert_eq!(
        kinds(hub),
        [
            (Agent, m4.as_str()),
            (Agent, b1.as_str()),
            (Agent, m5.as_str())
        ]
    );
    assert!(close(3.0 * OPUS, hub.metrics.cost_usd));
    // M6: unnamed, on a detached HEAD.
    let unlinked = child(&root, Unlinked, "");
    assert_eq!(kinds(unlinked), [(Agent, format!("{M6}/").as_str())]);
    // Every request once: 12 opus, 2 sonnet; 9 (session, charge) rows.
    assert!(close(12.0 * OPUS + 2.0 * SONNET, root.metrics.cost_usd));
    assert_eq!(root.metrics.sessions, 9);
    check_sums(&root, "model-home");
    // By role: a parsed name's role (M1 manager, a3 review, the M7 chain's
    // registry name impl, M4 `director-hub` by its legacy typed name director),
    // else a subagent's type (a1 and b1 implementer, a2 Explore), else
    // `root` for a root agent (M5).
    let roles: Vec<_> = split(&root.metrics, Split::Role)
        .iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(
        roles,
        [
            "Explore",
            "director",
            "impl",
            "implementer",
            "manager",
            "review",
            "root"
        ]
    );
    let usd = |m: &Metrics, key: &str| {
        m.by_role
            .iter()
            .find(|s| s.key == key)
            .map_or(0.0, |s| s.cost_usd)
    };
    assert!(close(2.0 * OPUS, usd(&root.metrics, "manager")));
    assert!(close(2.0 * OPUS, usd(&root.metrics, "impl")));
    assert!(close(SONNET, usd(&root.metrics, "review")));
    for kind in [Split::Day, Split::Role, Split::Model] {
        check_split_sums(&root, kind, "model-home");
        check_split_children(&root, kind, "model-home");
    }

    // The reviewer's node draws a3's run alone: 06:02:10 to 06:02:20 EDT.
    let path = steps(&[
        (Repo, "strate"),
        (Epic, "105"),
        (Issue, "107"),
        (Pr, "120"),
        (Agent, &a3),
    ]);
    let days = audit_timeline(&db, &day, &path).expect("timeline");
    let at = |s: i64| (6 * 3600 + 2 * 60 + s) * S;
    assert_eq!(
        days,
        [TimelineDay {
            day: "2026-10-20".into(),
            spans: vec![span(at(10), at(20), SpanKind::Agent)],
            marks: vec![],
        }]
    );
}

fn span(start_ms: i64, end_ms: i64, kind: SpanKind) -> TimelineSpan {
    TimelineSpan {
        start_ms,
        end_ms,
        kind,
    }
}

fn mark(at_ms: i64, label: &str) -> TimelineMark {
    TimelineMark {
        at_ms,
        label: label.into(),
    }
}

/// A node path from the root (the root itself not included).
fn steps(path: &[(NodeKind, &str)]) -> Vec<(NodeKind, String)> {
    path.iter().map(|&(k, key)| (k, key.to_string())).collect()
}

#[test]
fn a_root_agent_across_local_midnight_draws_on_both_days() {
    let t = Temp::new("timeline-hand");
    let db = build(&t);
    let tree = october(&db).root;
    let pr = child(issue(&tree, "9"), NodeKind::Pr, "12");
    let s2 = labelled(pr, NodeKind::Agent, "issue_demo-9");
    let oct = filter("2026-10-01", "2026-10-31", &[]);
    let to_pr = [
        (NodeKind::Repo, "demo"),
        (NodeKind::Epic, "8"),
        (NodeKind::Issue, "9"),
        (NodeKind::Pr, "12"),
    ];
    let mut path = steps(&to_pr);
    path.push((NodeKind::Agent, s2.key.clone()));
    let days = audit_timeline(&db, &oct, &path).expect("timeline");
    // issue_demo-9 starts 03:50:00Z Oct 5 = 23:50 EDT Oct 4, 85 800 s after
    // that day's local midnight; the next (04:00Z) falls 600 s in, inside
    // the idle [140,900]. The root agent: model [0,10], tools [10,15],
    // model [15,25] are one active span; the wait [25,900] is waiting until
    // the idle gap takes it at 140; req_b3 [900,910] is 300..310 s into Oct
    // 5. Its background subagent's run [20,140] is its own node's, not the
    // root agent's. An agent holds no issue or PR, so it has no marks.
    let oct4 = |s: i64| (85_800 + s) * S;
    let s2_oct4 = [
        span(oct4(0), oct4(25), SpanKind::Active),
        span(oct4(25), oct4(140), SpanKind::Wait),
    ];
    assert_eq!(
        days,
        [
            TimelineDay {
                day: "2026-10-04".into(),
                spans: s2_oct4.to_vec(),
                marks: vec![],
            },
            TimelineDay {
                day: "2026-10-05".into(),
                spans: vec![span(300 * S, 310 * S, SpanKind::Active)],
                marks: vec![],
            },
        ]
    );
    let mut to_bg = steps(&to_pr);
    to_bg.push((NodeKind::Agent, format!("{A2}/{BG}")));
    let bg_oct4 = span(oct4(20), oct4(140), SpanKind::Agent);
    assert_eq!(
        audit_timeline(&db, &oct, &to_bg).expect("timeline"),
        [TimelineDay {
            day: "2026-10-04".into(),
            spans: vec![bg_oct4.clone()],
            marks: vec![],
        }]
    );
    let s2_oct4 = [s2_oct4[0].clone(), bg_oct4, s2_oct4[1].clone()];

    // Issue 9 adds demo-9 (14:00:00Z Oct 4 = 10:00 EDT, 36 000 s in):
    // model [0,10], tools [10,120] and model [120,130] active, the wait
    // [130,310], model [310,320]; its subagent [20,80]. Marks at or under
    // the issue: PR #12 merged 11:00Z Oct 5 (07:00 EDT) and #9 closed 12:00Z
    // (08:00 EDT).
    let oct4_9 = |s: i64| (36_000 + s) * S;
    let mut spans = vec![
        span(oct4_9(0), oct4_9(130), SpanKind::Active),
        span(oct4_9(20), oct4_9(80), SpanKind::Agent),
        span(oct4_9(130), oct4_9(310), SpanKind::Wait),
        span(oct4_9(310), oct4_9(320), SpanKind::Active),
    ];
    spans.extend(s2_oct4);
    let days = audit_timeline(&db, &oct, &steps(&to_pr[..3])).expect("timeline");
    assert_eq!(
        days,
        [
            TimelineDay {
                day: "2026-10-04".into(),
                spans,
                marks: vec![],
            },
            TimelineDay {
                day: "2026-10-05".into(),
                spans: vec![span(300 * S, 310 * S, SpanKind::Active)],
                marks: vec![
                    mark(7 * 3600 * S, "PR #12 merged"),
                    mark(8 * 3600 * S, "#9 closed"),
                ],
            },
        ]
    );
    // The PR node holds the merge, not the issue's close above it.
    let days = audit_timeline(&db, &oct, &steps(&to_pr)).expect("timeline");
    assert_eq!(days[1].marks, [mark(7 * 3600 * S, "PR #12 merged")]);
}

#[test]
fn an_unknown_path_has_no_timeline_and_a_bad_day_is_an_error() {
    let t = Temp::new("timeline-unknown");
    let db = build(&t);
    let oct = filter("2026-10-01", "2026-10-31", &[]);
    for path in [
        steps(&[(NodeKind::Repo, "nope")]),
        steps(&[(NodeKind::Repo, "demo"), (NodeKind::Issue, "9")]),
        steps(&[
            (NodeKind::Repo, "demo"),
            (NodeKind::Epic, "8"),
            (NodeKind::Issue, "99"),
        ]),
    ] {
        assert_eq!(audit_timeline(&db, &oct, &path).expect("timeline"), []);
    }
    // The root's own step may lead the path.
    let demo = steps(&[(NodeKind::Repo, "demo")]);
    let mut rooted = steps(&[(NodeKind::Root, "")]);
    rooted.extend(demo.clone());
    let want = audit_timeline(&db, &oct, &demo).expect("timeline");
    assert!(!want.is_empty());
    assert_eq!(audit_timeline(&db, &oct, &rooted).expect("timeline"), want);
    let bad = filter("October", "2026-10-31", &[]);
    assert!(audit_timeline(&db, &bad, &demo).is_err());
}

/// Calls `visit` on `node` and every node under it, with its path.
fn walk(
    node: &AuditNode,
    path: &mut Vec<(NodeKind, String)>,
    visit: &mut impl FnMut(&[(NodeKind, String)], &AuditNode),
) {
    visit(path, node);
    for c in &node.children {
        path.push((c.kind, c.key.clone()));
        walk(c, path, visit);
        path.pop();
    }
}

#[test]
fn each_days_spans_sum_to_every_nodes_cells_that_day() {
    let t = Temp::new("timeline-sums");
    let db = build(&t);
    let days: Vec<String> = october(&db)
        .root
        .metrics
        .by_day
        .iter()
        .map(|s| s.key.clone())
        .collect();
    assert!(days.len() >= 2, "{days:?}");
    let mut checked = 0;
    for day in &days {
        let f = filter(day, day, &[]);
        let tree = audit(&db, &f).expect("audit").root;
        walk(&tree, &mut Vec::new(), &mut |path, node| {
            let got = audit_timeline(&db, &f, path).expect("timeline");
            assert!(got.iter().all(|d| &d.day == day), "{day} {path:?}");
            let total = |kind| -> i64 {
                got.iter()
                    .flat_map(|d| &d.spans)
                    .filter(|s| s.kind == kind)
                    .map(|s| s.end_ms - s.start_ms)
                    .sum()
            };
            let (active, wait, agent) = (
                total(SpanKind::Active),
                total(SpanKind::Wait),
                total(SpanKind::Agent),
            );
            let m = &node.metrics;
            let at = format!("{day} {path:?}");
            if is_subagent(node) {
                // A subagent's node: its own non-idle time.
                assert_eq!((active, wait, agent), (0, 0, m.active_ms), "{at}");
            } else {
                // Active time includes the waiting; agent-hours are the
                // subagents' wall, none of it idle in this fixture.
                assert_eq!(
                    (active + wait, wait, agent),
                    (m.active_ms, m.wait_ms, m.agent_ms),
                    "{at}"
                );
            }
            checked += 1;
        });
    }
    assert!(checked > 20, "{checked}");
}

#[test]
fn the_timeline_uses_the_webview_shape() {
    let path: Vec<(NodeKind, String)> =
        serde_json::from_value(serde_json::json!([["repo", "demo"], ["issue", "9"]]))
            .expect("path");
    assert_eq!(
        path,
        steps(&[(NodeKind::Repo, "demo"), (NodeKind::Issue, "9")])
    );
    let day = TimelineDay {
        day: "2026-10-05".into(),
        spans: vec![span(1, 2, SpanKind::Agent)],
        marks: vec![mark(3, "#9 closed")],
    };
    assert_eq!(
        serde_json::to_value(day).expect("json"),
        serde_json::json!({
            "day": "2026-10-05",
            "spans": [{"startMs": 1, "endMs": 2, "kind": "agent"}],
            "marks": [{"atMs": 3, "label": "#9 closed"}]
        })
    );
}
