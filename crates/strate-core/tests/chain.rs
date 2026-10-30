//! A `/clear` chain read as one tab (#63, #107), over the synthetic
//! `tests/fixtures/chain-home`: one tab per process, drawing its latest
//! link, a `--bg` unit's tab nested under its spawner's live tab, a
//! registry row read as its process's latest link (and a hook wait listed
//! on that link only), the graph drawing a spawner's live units only, with
//! no finished worker on any link (#81), and every Audit cell counted once.
//! Every db lives in a temp dir.

use std::path::PathBuf;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::hooks::{Hook, HookEvent};
use strate_core::registry::{RegistryEntry, RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{
    self, AgentState, AuditFilter, AuditNode, GraphAgent, GraphEdge, LiveGraph, Metrics,
    Workstream, audit,
};

/// director-chain's first link.
const L1: &str = "5e55c4c4-0000-4000-8000-0000000000a1";
/// director-chain's third and last link.
const L3: &str = "5e55c4c4-0000-4000-8000-0000000000a3";
/// ticket-chain-repo-7's first link, the one director-chain messaged.
const U1: &str = "5e55c4c4-0000-4000-8000-0000000000b1";
/// ticket-chain-repo-7's second link, after /clear: never messaged itself.
const U2: &str = "5e55c4c4-0000-4000-8000-0000000000b2";
/// chain-repo-5, then renamed chain-repo-6 and back over two /clears.
const P1: &str = "5e55c4c4-0000-4000-8000-0000000000c1";
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

fn close(want: f64, got: f64) -> bool {
    (want - got).abs() < 1e-9
}

fn tree(db: &Connection) -> AuditNode {
    let oct8 = AuditFilter {
        from: "2026-10-08".into(),
        to: "2026-10-08".into(),
        repos: vec![],
    };
    audit(db, &oct8).expect("audit").root
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
    let root = tree(&db);
    check_sums(&root);
    let m = &root.metrics;
    assert!(
        close(17.0 * OPUS + 8.0 * SONNET, m.cost_usd),
        "{}",
        m.cost_usd
    );
    assert_eq!(m.agents, 8);
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

/// The orchestrator row id of `session`: for a chain's first link, its
/// tab's id.
fn orchestrator(db: &Connection, session: &str) -> i64 {
    db.query_row(
        "SELECT id FROM agents WHERE session_id = ?1 AND agent_id IS NULL",
        [session],
        |r| r.get(0),
    )
    .expect("orchestrator")
}

/// The tab of the chain whose first link is `first`.
fn tab<'a>(db: &Connection, rows: &'a [Workstream], first: &str) -> &'a Workstream {
    let id = orchestrator(db, first);
    rows.iter()
        .find(|w| w.id == id)
        .unwrap_or_else(|| panic!("no tab for {first}: {rows:?}"))
}

/// The live graph of that tab.
fn graph(db: &Connection, first: &str) -> LiveGraph {
    views::live_graph(db, orchestrator(db, first))
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
fn a_clear_chain_is_one_tab_drawing_its_latest_link() {
    let t = Temp::new("tabs");
    let (_store, db) = build(&t);
    let rows = views::workstreams(&db).expect("workstreams");
    // Three processes, three tabs, whatever their links were renamed to.
    let mut shown: Vec<(&str, &str, bool, &str)> = rows
        .iter()
        .map(|w| {
            (
                w.session_id.as_deref().expect("drawn"),
                w.name.as_str(),
                w.fallback,
                w.started_at.as_deref().expect("started"),
            )
        })
        .collect();
    shown.sort();
    assert_eq!(
        shown,
        [
            (L3, "topic_chain", false, "2026-10-08T12:00:00.000Z"),
            (U2, "issue_chain-repo-7", false, "2026-10-08T11:30:00.000Z"),
            (P3, "issue_chain-repo-5", false, "2026-10-08T15:00:00.000Z"),
        ]
    );
    for (first, latest) in [(L1, L3), (U1, U2), (P1, P3)] {
        assert_eq!(tab(&db, &rows, first).session_id.as_deref(), Some(latest));
    }
    // Each link's subagent has answered, so only the orchestrator is drawn.
    let p = graph(&db, P1);
    assert_eq!(p.session_id, P3);
    assert_eq!(drawn(&p), [("-", "chain-repo-5")]);
}

#[test]
fn the_spawners_canvas_draws_its_live_units_latest_link_and_no_finished_subagent() {
    let t = Temp::new("graph");
    let (mut store, db) = build(&t);
    // The unit's process is not running: the canvas leaves it out. Every
    // link's subagent has answered, so none is drawn either.
    let g = graph(&db, L1);
    assert_eq!(g.session_id, L3);
    assert_eq!(drawn(&g), [("-", "director-chain")]);
    // The registry keys a background process by its first session, U1:
    // the unit drawn is its process's latest link, U2.
    store
        .apply_registry(&[listed(4202, U1, "ticket-chain-repo-7")])
        .expect("unit live");
    let g = graph(&db, L1);
    // L2, its spawner, is an earlier link: U2 hangs under the drawn L3.
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
fn the_header_and_the_tab_sum_every_link_of_the_chain() {
    let t = Temp::new("header");
    let (_store, db) = build(&t);
    let rows = views::workstreams(&db).expect("workstreams");
    let header = |first| {
        let g = graph(&db, first);
        let usd = g.session_cost_usd.expect("priced");
        let tab_usd = tab(&db, &rows, first).cost_usd.expect("priced");
        assert!(
            close(usd, tab_usd),
            "the latest link is drawn: {usd} {tab_usd}"
        );
        (usd, g.run_ms)
    };
    // L1, L2 (plus its SendMessage) and L3, each with its subagent; each
    // link's run: L1 and L3 :00:00 to :00:55, L2 11:00:00 to 11:02:01.
    let (usd, run) = header(L1);
    assert!(close(7.0 * OPUS + 3.0 * SONNET, usd), "{usd}");
    assert_eq!(run, Some((55 + 121 + 55) * S));
    // P1, P2 and P3, whatever each was named.
    let (usd, run) = header(P1);
    assert!(close(6.0 * OPUS + 3.0 * SONNET, usd), "{usd}");
    assert_eq!(run, Some(3 * 55 * S));
    // U1 from the message it received at 11:02:02, and U2.
    let (usd, run) = header(U1);
    assert!(close(4.0 * OPUS + 2.0 * SONNET, usd), "{usd}");
    assert_eq!(run, Some(168 * S));
    let costs = views::workstream_costs(&db).expect("costs");
    assert_eq!(costs.len(), 3, "{costs:?}");
}

#[test]
fn a_unit_tab_nests_whichever_link_of_its_chain_was_spawned() {
    let t = Temp::new("nest");
    let (mut store, db) = build(&t);
    store
        .apply_registry(&[
            listed(4201, L3, "director-chain"),
            listed(4202, U2, "ticket-chain-repo-7"),
        ])
        .expect("live");
    let rows = views::workstreams(&db).expect("workstreams");
    let unit = tab(&db, &rows, U1);
    // U2 is drawn; U1, its earlier link, was the one messaged, by L2.
    assert_eq!(unit.session_id.as_deref(), Some(U2));
    assert_eq!(unit.director, Some(tab(&db, &rows, L1).id));
    assert_eq!(tab(&db, &rows, P1).director, None);
}

/// A store holding exactly `sql`'s rows (no transcripts), and a reader.
/// Every session gets one orchestrator record, which makes it a link.
fn synthetic(t: &Temp, sql: &str) -> Connection {
    drop(Store::open_in(t.db(), "UTC").expect("open"));
    let raw = Connection::open(t.db()).expect("raw");
    raw.execute_batch(sql).expect("rows");
    raw.execute_batch(
        "INSERT INTO events (session_id, agent_id, file, byte_offset, raw)
         SELECT session_id, NULL, path, 0, '{}' FROM sessions",
    )
    .expect("records");
    Store::open_reader(t.db()).expect("reader")
}

/// d spawned y, u1 and u2, the links of one process (pu, in that order);
/// u1 dispatched k1.
const SPAWNED_CHAIN: &str = "
    INSERT INTO sessions (session_id, path, started_at, process_root) VALUES
        ('d', '/p/d.jsonl', '2026-10-02T09:00:00.000Z', 'd'),
        ('y', '/p/y.jsonl', '2026-10-02T09:30:00.000Z', 'pu'),
        ('u1', '/p/u1.jsonl', '2026-10-02T10:00:00.000Z', 'pu'),
        ('u2', '/p/u2.jsonl', '2026-10-02T11:00:00.000Z', 'pu');
    INSERT INTO agents (id, session_id, agent_id, kind) VALUES
        (1, 'd', NULL, 'orchestrator'), (2, 'u1', NULL, 'orchestrator'),
        (3, 'u2', NULL, 'orchestrator'), (4, 'u1', 'k1', 'subagent'),
        (5, 'y', NULL, 'orchestrator');
    INSERT INTO edges (kind, parent, child, tool_use_id) VALUES ('dispatch', 2, 4, 't1'),
        ('message', 1, 5, NULL), ('message', 1, 2, NULL), ('message', 1, 3, NULL);
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
fn a_unit_chain_spawned_at_every_link_draws_once_by_its_live_latest_link() {
    let t = Temp::new("unit-chain");
    let db = synthetic(&t, SPAWNED_CHAIN);
    // No unit process runs: the spawner is drawn alone.
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(placed(&g), [(1, None)]);
    // The registry lists the process by its first link, y: u2, its latest,
    // runs, drawn once. u1's k1, an earlier link's worker, reads done.
    background(&t, "y", "running");
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(placed(&g), [(1, None), (3, Some(1))]);
    let rows = views::workstreams(&db).expect("workstreams");
    let unit = rows.iter().find(|w| w.id == 5).expect("pu's tab, y's id");
    assert_eq!((unit.session_id.as_deref(), unit.live), (Some("u2"), true));
}

#[test]
fn a_blocked_background_unit_is_not_drawn() {
    let t = Temp::new("unit-blocked");
    let db = synthetic(&t, SPAWNED_CHAIN);
    // Blocked on the user, the unit's process is not running: the canvas
    // leaves it out, as it does a finished one.
    background(&t, "y", "blocked");
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(placed(&g), [(1, None)]);
}

/// Unit chain pk: da spawned k1, then db spawned k2. Only db is live.
const TWO_SPAWNERS: &str = "
    INSERT INTO sessions (session_id, path, started_at, process_root) VALUES
        ('da', '/p/da.jsonl', '2026-10-02T08:00:00.000Z', 'da'),
        ('db', '/p/db.jsonl', '2026-10-02T09:00:00.000Z', 'db'),
        ('k1', '/p/k1.jsonl', '2026-10-02T10:00:00.000Z', 'pk'),
        ('k2', '/p/k2.jsonl', '2026-10-02T11:00:00.000Z', 'pk');
    INSERT INTO agents (id, session_id, kind) VALUES (1, 'da', 'orchestrator'),
        (2, 'db', 'orchestrator'), (3, 'k1', 'orchestrator'), (4, 'k2', 'orchestrator');
    INSERT INTO edges (kind, parent, child) VALUES ('message', 1, 3), ('message', 2, 4);
    INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, first_seen_ms,
        last_seen_ms) VALUES ('pid:2:1', 'interactive', 'db', '/work/x', 1, 'busy', 1, 1);
";

#[test]
fn a_unit_tab_nests_under_a_live_spawner_of_any_of_its_links() {
    let t = Temp::new("two-spawners");
    let db = synthetic(&t, TWO_SPAWNERS);
    let director = |db: &Connection| {
        let rows = views::workstreams(db).expect("workstreams");
        rows.iter().find(|w| w.id == 3).expect("pk's tab").director
    };
    // k1's spawner is not live; k2's is.
    assert_eq!(director(&db), Some(2));
    // With both live, the earliest spawned link's spawner.
    Connection::open(t.db())
        .expect("raw")
        .execute_batch(
            "INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'da', '/work/x', 1, 'busy', 1, 1);",
        )
        .expect("live");
    assert_eq!(director(&db), Some(1));
}

/// One background process (root f): f named ticket-x-17, then /clear and
/// /rename into g, ticket-x-18. The registry keys the process by its first
/// session, f, and has named it ticket-x-18; it waits on a permission.
const RENAMED_PROCESS: &str = "
    INSERT INTO sessions (session_id, path, started_at, process_root) VALUES
        ('f', '/p/f.jsonl', '2026-10-02T10:00:00.000Z', 'f'),
        ('g', '/p/g.jsonl', '2026-10-02T11:00:00.000Z', 'f');
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
    let shown: Vec<_> = rows
        .iter()
        .map(|w| (w.id, w.session_id.as_deref(), w.live, w.needs_you, w.state))
        .collect();
    // One tab for the process, drawing its running link.
    assert_eq!(shown, [(1, Some("g"), true, 1, AgentState::NeedsYou)]);
    // The wait names the latest link and counts toward the chain's tab.
    let needs: Vec<(Option<String>, Option<i64>)> = views::needs_you(&db)
        .expect("needs you")
        .into_iter()
        .map(|n| (n.session_id, n.workstream))
        .collect();
    assert_eq!(needs, [(Some("g".to_string()), Some(1))]);
    // Either link's orchestrator id reads as the tab.
    for id in [1, 2] {
        let g = views::live_graph(&db, id).expect("query").expect("graph");
        assert!(g.live);
        assert_eq!(g.session_id, "g");
        assert_eq!(head(&g), (Some("ticket-x-18"), AgentState::NeedsYou));
    }

    Connection::open(t.db())
        .expect("raw")
        .execute(
            "UPDATE registry SET status = 'busy', waiting_for = NULL",
            [],
        )
        .expect("busy");
    let rows = views::workstreams(&db).expect("workstreams");
    assert_eq!(rows[0].state, AgentState::Working);
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
    assert_eq!(head(&g), (Some("ticket-x-18"), AgentState::Working));
}

#[test]
fn a_blocked_background_session_has_no_live_tab_but_needs_you() {
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
    let shown: Vec<_> = rows
        .iter()
        .map(|w| (w.id, w.session_id.as_deref(), w.live, w.needs_you, w.state))
        .collect();
    // Not live; the wait counts toward the chain's tab, drawing g.
    assert_eq!(shown, [(1, Some("g"), false, 1, AgentState::NeedsYou)]);
    let needs: Vec<(String, Option<String>, Option<i64>)> = views::needs_you(&db)
        .expect("needs you")
        .into_iter()
        .map(|n| (n.source, n.session_id, n.workstream))
        .collect();
    assert_eq!(
        needs,
        [("registry".to_string(), Some("g".to_string()), Some(1))]
    );
    let g = views::live_graph(&db, 1).expect("query").expect("graph");
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
    assert_eq!(tab(&db, &rows, U1).needs_you, 1);
    // The spawner's canvas draws U2's waiting subagent, not U1's done one.
    let g = graph(&db, L1);
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
