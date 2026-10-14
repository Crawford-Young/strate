//! Audit rollups (#26) over the synthetic `tests/fixtures/audit-home` and its
//! GitHub data (`tests/fixtures/github/audit-home.json`), with days in
//! America/Toronto. Hand values for the closed issue `demo-9`, a property
//! test (every additive metric of every node is the sum of its children,
//! under every group-by, and the root is the sum of the cells in the filter),
//! the date-range and repo filters, Unlinked, the background-dispatch flag
//! and the headline. The row timeline (`audit_timeline`): hand values for
//! demo-9 across a local midnight, and a property test (each day's spans
//! sum to every node's cells that day). A nested epic (#47) over
//! `tests/fixtures/nest-home` and `github/nest-home.json`. Every db lives in
//! a temp dir.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::github::{Runner, refresh};
use strate_core::store::Store;
use strate_core::store::views::{
    Audit, AuditFilter, AuditNode, GroupBy, NodeKind, SpanKind, TimelineDay, TimelineMark,
    TimelineSpan, audit, audit_timeline,
};
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

const TORONTO: &str = "America/Toronto";
const FG: &str = "a9000000000000001";
const BG: &str = "a9000000000000002";
const S: i64 = 1000;

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-rollup-{name}-{}", std::process::id()));
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

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The synthetic `~/.claude` at `tests/fixtures/<name>`.
fn home(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// Every transcript under `home/projects`, sorted.
fn transcripts(home: &Path) -> Vec<PathBuf> {
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
    walk(&home.join("projects"), &mut out);
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
    let home = home(name);
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    store
        .ingest_graph(&discover(&home).expect("discover"))
        .expect("graph");
    for path in transcripts(&home) {
        store.ingest(&batch(&path)).expect("ingest");
    }
    store
}

fn filter(from: &str, to: &str, repos: &[&str], group_by: GroupBy) -> AuditFilter {
    AuditFilter {
        from: from.into(),
        to: to.into(),
        repos: repos.iter().map(|r| r.to_string()).collect(),
        group_by,
    }
}

fn october(db: &Connection, group_by: GroupBy) -> Audit {
    audit(db, &filter("2026-10-01", "2026-10-31", &[], group_by)).expect("audit")
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
    let tree = october(&db, GroupBy::Hierarchy).root;
    let demo9 = issue(&tree, "9");
    let m = &demo9.metrics;
    // Two windows, each its own transcript (times Z, Toronto = Z - 4 h):
    // demo-9 (14:00:00..14:05:20 on Oct 4), relative seconds: prompt 0,
    // req_a1 10 (Agent toolu_a1), result 120, req_a2 130, prompt 310, req_a3
    // 320; its subagent at 20 and 80. Model [0,10] + [120,130] + [310,320],
    // tools [10,120], wait for the 310 prompt since the last work [130,310]
    // = 180 s; no gap over 10 min: active 320 s.
    // demo-9-s2 (03:50:00..04:05:10 on Oct 5): prompt 0, req_b1 10 (Agent,
    // background), result 15, req_b2 25, prompt 900, req_b3 910; its subagent
    // at 20 and 140. Model [0,10] + [15,25] + [900,910], tools [10,15], wait
    // [25,140] (the wait [25,900] loses [140,900] to idle: 760 s > 10 min).
    // Active 910 - 760 = 150 s, waiting 115 s.
    assert_eq!(m.active_ms, (320 + 150) * S);
    assert_eq!(m.wait_ms, (180 + 115) * S);
    // Subagents: [20,80] = 60 s and [20,140] = 120 s.
    assert_eq!(m.agent_ms, (60 + 120) * S);
    assert!(m.approximate, "the background subagent's hours");
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
    assert_eq!((m.sessions, m.agents, m.rotations), (2, 2, 1));
    // First window start Oct 4 14:00:00Z -> closed Oct 5 12:00:00Z = 22 h.
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

#[test]
fn windows_sit_under_their_pr_or_directly_under_the_issue() {
    let t = Temp::new("levels");
    let db = build(&t);
    let tree = october(&db, GroupBy::Hierarchy).root;
    let demo9 = issue(&tree, "9");
    assert_eq!(demo9.label, "#9 Ipsum rollup");
    // demo-9-s2 links PR #12; demo-9 falls back to the issue's only PR.
    assert_eq!(demo9.children.len(), 1);
    let pr = child(demo9, NodeKind::Pr, "12");
    assert!(!pr.metrics.open, "merged");
    // First window start Oct 4 14:00:00Z -> merged Oct 5 11:00:00Z.
    assert_eq!(pr.metrics.elapsed_ms, Some(21 * 3600 * S));
    let first = labelled(pr, NodeKind::Session, "demo-9");
    let second = labelled(pr, NodeKind::Session, "demo-9-s2");
    assert_eq!(first.metrics.elapsed_ms, Some(320 * S));
    assert_eq!(second.metrics.elapsed_ms, Some(910 * S));
    assert_eq!((first.metrics.rotations, second.metrics.rotations), (0, 1));

    // The agent level keeps its own active and waiting time.
    let orchestrator = child(first, NodeKind::Agent, "");
    let sub = child(first, NodeKind::Agent, FG);
    assert_eq!(orchestrator.metrics.active_ms, 320 * S);
    assert_eq!(orchestrator.metrics.agent_ms, 0);
    assert_eq!(
        (sub.metrics.active_ms, sub.metrics.agent_ms),
        (60 * S, 60 * S)
    );
    assert_eq!(sub.metrics.elapsed_ms, Some(60 * S));
    assert_eq!((sub.metrics.agents, orchestrator.metrics.agents), (1, 0));
    assert_eq!(first.metrics.active_ms, 320 * S, "not 320 + 60");

    // #10 is open with no PR: its window sits directly under it.
    let demo10 = issue(&tree, "10");
    let window = labelled(demo10, NodeKind::Session, "demo-10");
    assert!(demo10.metrics.open);
    assert_eq!(demo10.metrics.elapsed_ms, Some(30 * S));
    assert!(close(OPUS, window.metrics.cost_usd));
}

#[test]
fn an_epic_with_its_own_window_is_one_node_with_direct_work() {
    let t = Temp::new("epic-direct");
    let db = build(&t);
    let tree = october(&db, GroupBy::Hierarchy).root;
    let demo = child(&tree, NodeKind::Repo, "demo");
    // #8 is an epic, so its window is not an `Issue 8` beside it.
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
    let window = labelled(direct, NodeKind::Session, "demo-8");
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
    let tree = october(&db, GroupBy::Hierarchy).root;
    let nest = child(&tree, NodeKind::Repo, "nest");
    // #5's only sub-issue is in another repo: an issue, not an epic whose
    // only child is its direct work.
    assert_eq!(kinds(nest), [(NodeKind::Epic, "1"), (NodeKind::Issue, "5")]);
    let five = child(nest, NodeKind::Issue, "5");
    assert_eq!(five.label, "#5 Amet abroad");
    labelled(five, NodeKind::Session, "nest-5");

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
    labelled(direct, NodeKind::Session, "nest-2");
    labelled(
        child(two, NodeKind::Issue, "3"),
        NodeKind::Session,
        "nest-3",
    );

    // One opus request per window: #2 holds its own and #3's, #1 also #4's.
    assert!(close(2.0 * OPUS, two.metrics.cost_usd));
    assert!(close(3.0 * OPUS, one.metrics.cost_usd));
    assert_eq!((two.metrics.sessions, one.metrics.sessions), (2, 3));
    // Open: nest-2's start (Oct 4 14:00:00Z) to #3's close (Oct 5
    // 18:00:00Z), its latest child end.
    assert!(two.metrics.open);
    assert_eq!(two.metrics.elapsed_ms, Some(28 * 3600 * S));

    // The timeline finds the nested epic by the same path.
    let oct = filter("2026-10-01", "2026-10-31", &[], GroupBy::Hierarchy);
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
         INSERT INTO workstreams (id, name, fallback) VALUES (1, 'ring-1', 0);
         INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id)
             VALUES (1, 's1', 0, '2026-10-02T10:00:00.000Z', 1);
         INSERT INTO audit_cells (session_id, agent_id, segment_id, day, model, wall_ms,
             active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
             cache_read_tokens, cache_write_tokens) VALUES
             ('s1', NULL, 1, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0);
         INSERT INTO gh_repos (slug, owner, name, fetched_at) VALUES ('ring', 'example', 'ring', 1);
         INSERT INTO gh_issues (repo, number, title, state, created_at, parent, sub_total,
             sub_completed) VALUES
             ('ring', 1, 'Lorem', 'OPEN', '2026-10-01T00:00:00Z', 2, 1, 0),
             ('ring', 2, 'Ipsum', 'OPEN', '2026-10-01T00:00:00Z', 1, 1, 0);",
    );
    let tree = lab(&db, "2026-10-01", "2026-10-31", GroupBy::Hierarchy).root;
    let two = child(child(&tree, NodeKind::Repo, "ring"), NodeKind::Epic, "2");
    let one = child(two, NodeKind::Epic, "1");
    assert_eq!(kinds(one), [(NodeKind::Issue, "1")]);
    assert_eq!(count(&tree, NodeKind::Epic, "1"), 1);
    assert!(close(1.0, one.metrics.cost_usd));
}

#[test]
fn only_unparsed_windows_are_unlinked() {
    let t = Temp::new("unlinked");
    let db = build(&t);
    let tree = october(&db, GroupBy::Hierarchy).root;
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
    assert_eq!(unlinked.children[0].kind, NodeKind::Session);
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
    labelled(three, NodeKind::Session, "other-3");
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
    let a = october(&db, GroupBy::Hierarchy);
    let demo = child(&a.root, NodeKind::Repo, "demo");
    let kinds: Vec<_> = demo
        .children
        .iter()
        .map(|c| (c.kind, c.key.as_str()))
        .collect();
    // With no cache nothing says #8 is an epic: its window is an issue too.
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
    // Both windows directly under the issue, no PR level.
    assert!(demo9.children.iter().all(|c| c.kind == NodeKind::Session));
    // The hand values of the cached build, unchanged.
    let m = &demo9.metrics;
    assert_eq!(
        (m.active_ms, m.wait_ms, m.agent_ms),
        (470 * S, 295 * S, 180 * S)
    );
    assert!(close(0.065, m.cost_usd) && m.approximate);
    let tk = &m.tokens;
    assert_eq!(
        (tk.input, tk.output, tk.cache_read, tk.cache_write),
        (7000, 1000, 70_000, 2000)
    );
    assert_eq!((m.sessions, m.agents, m.rotations), (2, 2, 1));
    // Open: Oct 4 14:00:00Z to demo-9-s2's last record, Oct 5 04:05:10Z.
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
fn a_background_dispatch_flags_its_agent_hours_approximate() {
    let t = Temp::new("background");
    let db = build(&t);
    let tree = october(&db, GroupBy::Hierarchy).root;
    let pr = child(issue(&tree, "9"), NodeKind::Pr, "12");
    let first = labelled(pr, NodeKind::Session, "demo-9");
    let second = labelled(pr, NodeKind::Session, "demo-9-s2");
    assert!(!child(first, NodeKind::Agent, FG).metrics.approximate);
    assert!(!first.metrics.approximate);
    let bg = child(second, NodeKind::Agent, BG);
    assert!(bg.metrics.approximate);
    assert_eq!(bg.metrics.agent_ms, 120 * S);
    assert!(second.metrics.approximate && tree.metrics.approximate);
    assert!(!issue(&tree, "10").metrics.approximate);
}

#[test]
fn a_background_dispatch_with_no_flag_is_known_by_its_launch() {
    let t = Temp::new("background-no-flag");
    let db = build_nest(&t);
    let tree = october(&db, GroupBy::Hierarchy).root;
    let five = child(child(&tree, NodeKind::Repo, "nest"), NodeKind::Issue, "5");
    let window = labelled(five, NodeKind::Session, "nest-5");
    // No run_in_background on its Agent call; its tool_result is the async
    // launch: [6,9] s.
    let bg = child(window, NodeKind::Agent, "a7000000000000001");
    assert!(bg.metrics.approximate);
    assert_eq!(bg.metrics.agent_ms, 3 * S);
    assert!(!child(window, NodeKind::Agent, "").metrics.approximate);
    assert!(window.metrics.approximate && five.metrics.approximate);
    let nest = child(&tree, NodeKind::Repo, "nest");
    assert!(!child(nest, NodeKind::Epic, "1").metrics.approximate);
}

#[test]
fn the_date_range_keeps_the_cells_of_its_local_days() {
    let t = Temp::new("dates");
    let db = build(&t);
    let day = |d: &str| {
        audit(&db, &filter(d, d, &[], GroupBy::Hierarchy))
            .expect("audit")
            .root
    };
    // Local midnight is Oct 5 04:00:00Z, 600 s into demo-9-s2: Oct 4 has
    // [0,600] (active 600 - idle [140,600] = 140 s, waiting [25,140]),
    // Oct 5 has [600,910] (active 910 - 900 = 10 s, idle the rest).
    let oct4 = day("2026-10-04");
    let m = &issue(&oct4, "9").metrics;
    assert_eq!((m.active_ms, m.wait_ms), ((320 + 140) * S, (180 + 115) * S));
    assert_eq!(m.agent_ms, 180 * S);
    // Everything but req_b3 (04:05:10Z, Oct 5 local).
    assert!(close(0.018 + 4.0 * OPUS + 2.0 * SONNET, m.cost_usd));
    assert_eq!((m.sessions, m.rotations), (2, 1));
    let oct5 = day("2026-10-05");
    let m = &issue(&oct5, "9").metrics;
    assert_eq!((m.active_ms, m.wait_ms, m.agent_ms), (10 * S, 0, 0));
    assert!(close(OPUS, m.cost_usd));
    assert_eq!((m.sessions, m.agents, m.rotations), (1, 0, 1));
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
        audit(
            &db,
            &filter("2026-10-01", "2026-10-31", repos, GroupBy::Hierarchy),
        )
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
fn group_bys_put_the_hierarchy_under_each_group() {
    let t = Temp::new("groups");
    let db = build(&t);
    let keys = |a: &Audit| -> Vec<String> {
        assert!(a.root.children.iter().all(|c| c.kind == NodeKind::Group));
        a.root.children.iter().map(|c| c.key.clone()).collect()
    };
    let days = october(&db, GroupBy::Day);
    assert_eq!(keys(&days), ["2026-10-04", "2026-10-05"]);
    // demo-9-s2 split at local midnight.
    let s2 = |day: &str| {
        let group = child(&days.root, NodeKind::Group, day);
        let pr = child(issue(group, "9"), NodeKind::Pr, "12");
        labelled(pr, NodeKind::Session, "demo-9-s2").metrics.clone()
    };
    let (a, b) = (s2("2026-10-04"), s2("2026-10-05"));
    assert_eq!((a.active_ms, a.wait_ms), (140 * S, 115 * S));
    assert_eq!((b.active_ms, b.wait_ms), (10 * S, 0));
    assert_eq!((a.sessions, b.sessions), (1, 0), "counted where it starts");

    assert_eq!(
        keys(&october(&db, GroupBy::Model)),
        ["claude-opus-5-5", "claude-sonnet-5-5"]
    );
    let types = october(&db, GroupBy::AgentType);
    assert_eq!(keys(&types), ["implementer", "orchestrator", "recon"]);
    let recon = child(&types.root, NodeKind::Group, "recon");
    assert_eq!(recon.metrics.agent_ms, 120 * S);
    assert!(recon.metrics.approximate);
    let prs = october(&db, GroupBy::Pr);
    assert_eq!(keys(&prs), ["", "demo#12"]);
    assert!(close(
        0.065,
        child(&prs.root, NodeKind::Group, "demo#12")
            .metrics
            .cost_usd
    ));
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
    assert_eq!(m.rotations, sum(|k| k.rotations), "{path}: rotations");
    assert_eq!(
        m.approximate,
        kids().any(|k| k.approximate),
        "{path}: approximate"
    );
    // Agents run in parallel: a session is not the sum of its agents.
    if node.kind != NodeKind::Session {
        assert_eq!(m.active_ms, sum(|k| k.active_ms), "{path}: active");
        assert_eq!(m.wait_ms, sum(|k| k.wait_ms), "{path}: waiting");
    }
    for c in &node.children {
        check_sums(c, &path);
    }
}

/// The cells in `[from, to]` that are not idle only, summed: $, the 4 token counts, agent ms
/// (subagent wall), active and waiting (orchestrator cells), windows and
/// subagents.
fn cell_totals(db: &Connection, from: &str, to: &str) -> (f64, [i64; 4], [i64; 3], [i64; 2]) {
    db.query_row(
        "SELECT ifnull(sum(cost_usd), 0), sum(input_tokens), sum(output_tokens),
             sum(cache_read_tokens), sum(cache_write_tokens),
             sum(CASE WHEN agent_id IS NOT NULL THEN wall_ms ELSE 0 END),
             sum(CASE WHEN agent_id IS NULL THEN active_ms ELSE 0 END),
             sum(CASE WHEN agent_id IS NULL THEN wait_ms ELSE 0 END),
             count(DISTINCT ifnull('w' || segment_id, session_id)),
             count(DISTINCT session_id || '/' || agent_id)
         FROM audit_cells WHERE day BETWEEN ?1 AND ?2
             AND NOT (active_ms = 0 AND wait_ms = 0 AND cost_usd IS NULL AND unpriced = 0
                 AND input_tokens + output_tokens + cache_read_tokens + cache_write_tokens = 0
                 AND (agent_id IS NULL OR wall_ms = 0))",
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
fn every_parent_is_the_sum_of_its_children_under_every_group_by() {
    let t = Temp::new("property");
    check_all_sums(&build(&t));
}

/// The property over 3 ranges and every group-by.
fn check_all_sums(db: &Connection) {
    for (from, to) in [
        ("2026-10-01", "2026-10-31"),
        ("2026-10-04", "2026-10-04"),
        ("2026-10-05", "2026-10-05"),
    ] {
        let (usd, tokens, [agent_ms, active, wait], [sessions, agents]) = cell_totals(db, from, to);
        assert!(sessions > 0, "the range has work");
        for group_by in [
            GroupBy::Hierarchy,
            GroupBy::Day,
            GroupBy::Pr,
            GroupBy::AgentType,
            GroupBy::Model,
        ] {
            let tree = audit(db, &filter(from, to, &[], group_by))
                .expect("audit")
                .root;
            check_sums(&tree, &format!("{from}..{to} {group_by:?}"));
            let m = &tree.metrics;
            assert!(close(usd, m.cost_usd), "{group_by:?} root $");
            let t = &m.tokens;
            assert_eq!(tokens, [t.input, t.output, t.cache_read, t.cache_write]);
            assert_eq!(
                [agent_ms, active, wait],
                [m.agent_ms, m.active_ms, m.wait_ms]
            );
            assert_eq!([sessions, agents], [m.sessions, m.agents]);
        }
    }
}

#[test]
fn the_headline_counts_closed_issues_and_merged_prs() {
    let t = Temp::new("headline");
    let db = build(&t);
    let h = october(&db, GroupBy::Model).headline;
    assert_eq!(h.issues_closed, 1);
    assert_eq!(h.median_elapsed_ms, Some(22 * 3600 * S));
    assert!(close(0.065, h.usd_per_closed_issue.expect("closed issue")));
    assert!(close(0.065, h.usd_per_merged_pr.expect("merged pr")));
    assert!(h.cloud_uncounted, "#29");
    // Oct 4 only: the same issue, with its Oct 4 $.
    let oct4 = audit(
        &db,
        &filter("2026-10-04", "2026-10-04", &[], GroupBy::Hierarchy),
    )
    .expect("audit")
    .headline;
    assert!(close(0.057, oct4.usd_per_closed_issue.expect("closed")));
    let none = audit(
        &db,
        &filter("2026-10-01", "2026-10-31", &["other"], GroupBy::Hierarchy),
    )
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
    assert!(audit(&db, &filter("2026-10", "2026-10-31", &[], GroupBy::Day)).is_err());
    assert!(audit(&db, &filter("2026-10-01", "lorem", &[], GroupBy::Day)).is_err());
}

#[test]
fn the_filter_and_tree_use_the_webview_shape() {
    let f: AuditFilter = serde_json::from_value(serde_json::json!({
        "from": "2026-10-01", "to": "2026-10-31", "repos": ["demo"], "groupBy": "agent_type"
    }))
    .expect("filter");
    assert_eq!(f.group_by, GroupBy::AgentType);
    let t = Temp::new("shape");
    let db = build(&t);
    let value = serde_json::to_value(october(&db, GroupBy::Hierarchy)).expect("json");
    let mut keys: Vec<_> = value.as_object().expect("audit").keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["githubErrors", "headline", "root"]);
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
            "approximate",
            "costUsd",
            "elapsedMs",
            "open",
            "progress",
            "rotations",
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
    let mut keys: Vec<_> = root.as_object().expect("node").keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["children", "github", "key", "kind", "label", "metrics"]
    );
    assert_eq!(root["children"][2]["kind"], "unlinked");
}

#[test]
fn the_headline_elapsed_is_the_whole_lifetime_whatever_the_range() {
    let t = Temp::new("lifetime");
    let db = build(&t);
    // Oct 5 holds only demo-9-s2's tail, but #9's elapsed still runs from
    // its first window ever (Oct 4 14:00:00Z) to the close (Oct 5 12:00:00Z).
    let oct5 = audit(
        &db,
        &filter("2026-10-05", "2026-10-05", &[], GroupBy::Hierarchy),
    )
    .expect("audit")
    .headline;
    assert_eq!(oct5.issues_closed, 1);
    assert_eq!(oct5.median_elapsed_ms, Some(22 * 3600 * S));
    assert!(close(OPUS, oct5.usd_per_closed_issue.expect("closed")));
    // A range with no work on #9 does not count it.
    let none = audit(
        &db,
        &filter("2026-10-06", "2026-10-31", &[], GroupBy::Hierarchy),
    )
    .expect("audit")
    .headline;
    assert_eq!((none.issues_closed, none.median_elapsed_ms), (0, None));
}

#[test]
fn the_audit_lists_the_repos_github_could_not_fetch() {
    let t = Temp::new("gh-errors");
    let db = build(&t);
    let errors = october(&db, GroupBy::Hierarchy).github_errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].repo, "other");
    assert!(errors[0].error.contains("Could not resolve"), "{errors:?}");
    let t = Temp::new("gh-errors-none");
    drop(build_without_gh(&t, "audit-home"));
    let db = Store::open_reader(t.db()).expect("reader");
    assert!(october(&db, GroupBy::Hierarchy).github_errors.is_empty());
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
/// with $2; #4 not cached, one window Friday to Monday, idle on the
/// weekend.
const LAB: &str = "
    INSERT INTO sessions (session_id, path, started_at) VALUES
        ('s1', '/p/s1.jsonl', '2026-10-02T10:00:00.000Z'),
        ('s2', '/p/s2.jsonl', '2026-10-02T12:00:00.000Z'),
        ('s3', '/p/s3.jsonl', '2026-10-02T14:00:00.000Z'),
        ('s4', '/p/s4.jsonl', '2026-10-09T20:00:00.000Z');
    INSERT INTO workstreams (id, name, fallback) VALUES
        (1, 'lab-1', 0), (2, 'lab-2', 0), (3, 'lab-3', 0), (4, 'lab-4', 0);
    INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
        (1, 's1', 0, '2026-10-02T10:00:00.000Z', 1),
        (2, 's2', 0, '2026-10-02T12:00:00.000Z', 2),
        (3, 's3', 0, '2026-10-02T14:00:00.000Z', 3),
        (4, 's4', 0, '2026-10-09T20:00:00.000Z', 4);
    INSERT INTO audit_cells (session_id, agent_id, segment_id, day, model, wall_ms,
        active_ms, wait_ms, cost_usd, unpriced, input_tokens, output_tokens,
        cache_read_tokens, cache_write_tokens) VALUES
        ('s1', NULL, 1, '2026-10-02', 'm', 1000, 1000, 0, 1.0, 0, 10, 1, 0, 0),
        ('s2', NULL, 2, '2026-10-02', 'm', 1000, 1000, 0, 3.0, 0, 10, 1, 0, 0),
        ('s3', NULL, 3, '2026-10-02', 'm', 1000, 1000, 0, 2.0, 0, 10, 1, 0, 0),
        ('s4', NULL, 4, '2026-10-09', 'm', 3600000, 600000, 0, 0.5, 0, 10, 1, 0, 0),
        ('s4', NULL, 4, '2026-10-10', NULL, 86400000, 0, 0, NULL, 0, 0, 0, 0, 0),
        ('s4', NULL, 4, '2026-10-11', NULL, 86400000, 0, 0, NULL, 0, 0, 0, 0, 0),
        ('s4', NULL, 4, '2026-10-12', 'm', 3600000, 600000, 0, 0.5, 0, 10, 1, 0, 0);
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

fn lab(db: &Connection, from: &str, to: &str, group_by: GroupBy) -> Audit {
    audit(db, &filter(from, to, &[], group_by)).expect("audit")
}

#[test]
fn a_pr_closing_two_issues_counts_once_in_dollars_per_merged_pr() {
    let t = Temp::new("pr-twice");
    let db = synthetic(&t, LAB);
    let h = lab(&db, "2026-10-01", "2026-10-31", GroupBy::Hierarchy).headline;
    // One PR, $1 + $3 of work under it: not two PRs at $1 and $3.
    assert!(close(4.0, h.usd_per_merged_pr.expect("merged")), "{h:?}");
}

#[test]
fn only_issues_closed_as_completed_count_in_the_headline() {
    let t = Temp::new("completed");
    let db = synthetic(&t, LAB);
    let a = lab(&db, "2026-10-01", "2026-10-31", GroupBy::Hierarchy);
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
fn the_pr_group_is_keyed_by_the_prs_own_repo() {
    let t = Temp::new("pr-repo");
    let db = synthetic(&t, LAB);
    let a = lab(&db, "2026-10-01", "2026-10-31", GroupBy::Pr);
    let keys: Vec<_> = a.root.children.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["", "lab-ui#5"]);
    let pr = child(&a.root, NodeKind::Group, "lab-ui#5");
    assert_eq!(pr.label, "lab-ui #5");
    assert!(close(4.0, pr.metrics.cost_usd));
}

#[test]
fn idle_only_days_show_nothing() {
    let t = Temp::new("idle");
    let db = synthetic(&t, LAB);
    let sat = lab(&db, "2026-10-10", "2026-10-10", GroupBy::Hierarchy).root;
    assert!(sat.children.is_empty(), "{:?}", sat.children);
    assert_eq!(sat.metrics.sessions, 0);
    let weekend = lab(&db, "2026-10-10", "2026-10-11", GroupBy::Day).root;
    assert!(weekend.children.is_empty());
    // Friday to Monday: the window, counted once, with its two working days.
    let four = lab(&db, "2026-10-09", "2026-10-12", GroupBy::Day).root;
    let days: Vec<_> = four.children.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(days, ["2026-10-09", "2026-10-12"]);
    assert_eq!(four.metrics.sessions, 1);
    assert!(close(1.0, four.metrics.cost_usd));
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
fn a_window_across_local_midnight_draws_on_both_days() {
    let t = Temp::new("timeline-hand");
    let db = build(&t);
    let tree = october(&db, GroupBy::Hierarchy).root;
    let pr = child(issue(&tree, "9"), NodeKind::Pr, "12");
    let s2 = labelled(pr, NodeKind::Session, "demo-9-s2");
    let oct = filter("2026-10-01", "2026-10-31", &[], GroupBy::Hierarchy);
    let to_pr = [
        (NodeKind::Repo, "demo"),
        (NodeKind::Epic, "8"),
        (NodeKind::Issue, "9"),
        (NodeKind::Pr, "12"),
    ];
    let mut path = steps(&to_pr);
    path.push((NodeKind::Session, s2.key.clone()));
    let days = audit_timeline(&db, &oct, &path).expect("timeline");
    // demo-9-s2 starts 03:50:00Z Oct 5 = 23:50 EDT Oct 4, 85 800 s after
    // that day's local midnight; the next (04:00Z) falls 600 s in, inside
    // the idle [140,900]. Orchestrator: model [0,10], tools [10,15], model
    // [15,25] are one active span; the wait [25,900] is waiting until the
    // idle gap takes it at 140; req_b3 [900,910] is 300..310 s into Oct 5.
    // The background subagent runs [20,140]. A session holds no issue or
    // PR, so it has no marks.
    let oct4 = |s: i64| (85_800 + s) * S;
    let s2_oct4 = [
        span(oct4(0), oct4(25), SpanKind::Active),
        span(oct4(20), oct4(140), SpanKind::Agent),
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
fn a_day_rows_timeline_keeps_to_its_own_day() {
    let t = Temp::new("timeline-day-group");
    let db = build(&t);
    let oct = filter("2026-10-01", "2026-10-31", &[], GroupBy::Day);
    // demo-9 works on Oct 4 and 5 (local); its PR merges and the issue closes
    // on Oct 5. The Oct 4 row holds neither, and draws nothing on Oct 5.
    let oct4 =
        audit_timeline(&db, &oct, &steps(&[(NodeKind::Group, "2026-10-04")])).expect("timeline");
    assert!(!oct4.is_empty());
    assert!(oct4.iter().all(|d| d.day == "2026-10-04"), "{oct4:?}");
    assert!(oct4.iter().all(|d| d.marks.is_empty()), "{oct4:?}");
    // The Oct 5 row keeps both marks.
    let oct5 =
        audit_timeline(&db, &oct, &steps(&[(NodeKind::Group, "2026-10-05")])).expect("timeline");
    let marks: Vec<_> = oct5.iter().flat_map(|d| d.marks.clone()).collect();
    assert_eq!(
        marks,
        [
            mark(7 * 3600 * S, "PR #12 merged"),
            mark(8 * 3600 * S, "#9 closed"),
        ]
    );
    assert!(oct5.iter().all(|d| d.day == "2026-10-05"), "{oct5:?}");
}

#[test]
fn an_unknown_path_has_no_timeline_and_a_bad_day_is_an_error() {
    let t = Temp::new("timeline-unknown");
    let db = build(&t);
    let oct = filter("2026-10-01", "2026-10-31", &[], GroupBy::Hierarchy);
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
    let bad = filter("October", "2026-10-31", &[], GroupBy::Hierarchy);
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
    let days: Vec<String> = october(&db, GroupBy::Day)
        .root
        .children
        .iter()
        .map(|c| c.key.clone())
        .collect();
    assert!(days.len() >= 2, "{days:?}");
    let mut checked = 0;
    for day in &days {
        for group_by in [GroupBy::Hierarchy, GroupBy::Model] {
            let f = filter(day, day, &[], group_by);
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
                let at = format!("{day} {group_by:?} {path:?}");
                if node.kind == NodeKind::Agent && !node.key.is_empty() {
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
