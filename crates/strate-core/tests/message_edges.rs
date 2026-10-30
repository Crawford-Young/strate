//! `message` spawn edges (#53, generic since #107) over the synthetic
//! `tests/fixtures/directs-home`: the msg_id a session sent and the peer
//! msg_id it received, and the edges rebuilt from them (whatever either
//! side is named, and whatever order the transcripts arrive in); and the
//! views over them: a spawned chain's tab nested under its spawner's live
//! tab, and the spawned sessions on its canvas. Every db lives in a temp
//! dir.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{self, AgentState, GraphEdge, Workstream};
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

/// director-demo.
const R1: &str = "5e55d7d7-0000-4000-8000-0000000000c1";
/// director-demo after /clear: R1's successor in its process.
const R2: &str = "5e55d7d7-0000-4000-8000-0000000000c0";
/// ticket-demo-repo-1, messaged by R1 first, then by R2.
const N1: &str = "5e55d7d7-0000-4000-8000-0000000000e1";
/// ticket-demo-repo-2, messaged from R1's subagent; never replies.
const N2: &str = "5e55d7d7-0000-4000-8000-0000000000e2";
/// ticket-demo-repo-2 again, a session of its own nobody messaged.
const N3: &str = "5e55d7d7-0000-4000-8000-0000000000e3";
/// general-notes: messages Q3 first, then Q3 messages it back.
const Q1: &str = "5e55d7d7-0000-4000-8000-0000000000a1";
/// ticket-demo-repo-3.
const Q3: &str = "5e55d7d7-0000-4000-8000-0000000000a3";
/// director-alpha, messaged back by X2 (a cycle).
const X1: &str = "5e55d7d7-0000-4000-8000-0000000000b1";
/// director-beta: spawned by X1, spawns N4.
const X2: &str = "5e55d7d7-0000-4000-8000-0000000000b2";
/// ticket-demo-repo-4, spawned by X2.
const N4: &str = "5e55d7d7-0000-4000-8000-0000000000e4";
/// R1's subagent.
const S1: &str = "a7000000000000001";

/// Each tab's first link, by a short label (R is the R1 -> R2 chain).
const TABS: [(&str, &str); 9] = [
    ("R", R1),
    ("N1", N1),
    ("N2", N2),
    ("N3", N3),
    ("Q1", Q1),
    ("Q3", Q3),
    ("X1", X1),
    ("X2", X2),
    ("N4", N4),
];

fn msg(n: u8) -> String {
    format!("5e55ee00-0000-4000-8000-{n:012x}")
}

mod common;

use common::{Temp, fixture_home, records as records_in, transcripts as transcripts_in};

fn root() -> PathBuf {
    fixture_home("directs-home")
}

/// Every transcript under `root/projects`, sorted.
fn transcripts() -> Vec<PathBuf> {
    transcripts_in(&root())
}

/// One record event per line of `path`, with its byte offset.
fn records(path: &Path) -> Vec<TailEvent> {
    records_in(path, 0..usize::MAX)
        .into_iter()
        .map(|(event, _)| event)
        .collect()
}

/// `events` as one batch. Unlike `common::batch` it takes any run of
/// records, down to one, and checkpoints one byte past the last one's
/// offset rather than at the end of its line.
fn batch(events: Vec<TailEvent>) -> Batch {
    let TailEvent::Record { path, offset, .. } = events.last().expect("non-empty") else {
        unreachable!("records only")
    };
    Batch {
        path: path.clone(),
        checkpoint: Some(Checkpoint {
            offset: offset + 1,
            identity: FileIdentity {
                volume: 0,
                index: 0,
            },
        }),
        events,
    }
}

/// A store at `t` fed the discovery graph, then `batches` in order.
fn build(t: &Temp, batches: impl IntoIterator<Item = Batch>) -> Connection {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    for b in batches {
        store.ingest(&b).expect("ingest");
    }
    drop(store);
    Connection::open(t.db()).expect("db")
}

/// Each transcript as one batch, in `order`.
fn whole_files(order: &[PathBuf]) -> Vec<Batch> {
    order.iter().map(|p| batch(records(p))).collect()
}

/// A `message` edge: (spawner session, spawner agent id, spawned session).
type Spawn = (String, Option<String>, String);

/// Every `message` edge, by spawned session.
fn spawns(conn: &Connection) -> Vec<Spawn> {
    conn.prepare(
        "SELECT p.session_id, p.agent_id, c.session_id FROM edges AS e
         JOIN agents AS p ON p.id = e.parent JOIN agents AS c ON c.id = e.child
         WHERE e.kind = 'message' AND c.agent_id IS NULL ORDER BY c.session_id",
    )
    .expect("prepare")
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .expect("query")
    .map(|e| e.expect("edge"))
    .collect()
}

fn spawn(parent: &str, agent: Option<&str>, child: &str) -> Spawn {
    (parent.into(), agent.map(str::to_string), child.into())
}

/// The edges the fixture encodes, by spawned session id.
fn want() -> Vec<Spawn> {
    vec![
        spawn(Q1, None, Q3),
        spawn(X1, None, X2),
        spawn(R1, None, N1),
        spawn(R1, Some(S1), N2),
        spawn(X2, None, N4),
    ]
}

#[test]
fn a_session_hangs_under_its_first_messager_whatever_either_is_named() {
    let t = Temp::new("edges");
    let conn = build(&t, whole_files(&transcripts()));
    // general-notes spawns ticket-demo-repo-3 with no director's name; R1's
    // subagent spawns N2. Not edges: N1's and Q3's replies and X2's message
    // back (each would close a cycle), R1's message to a session not on
    // disk, and R2's later message to N1 (the first messager wins).
    assert_eq!(spawns(&conn), want());
}

#[test]
fn sent_and_received_msg_ids_are_stored_on_their_events() {
    let t = Temp::new("columns");
    let conn = build(&t, whole_files(&transcripts()));
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).expect(sql) };
    // m1..m10 sent; all but m4 (to a session not on disk) received.
    assert_eq!(
        count("SELECT count(DISTINCT sent_msg_id) FROM events WHERE sent_msg_id IS NOT NULL"),
        10
    );
    assert_eq!(
        count("SELECT count(*) FROM events WHERE peer_msg_id IS NOT NULL"),
        9
    );
    // A subagent's send belongs to its session.
    let (session, agent): (String, Option<String>) = conn
        .query_row(
            "SELECT session_id, agent_id FROM events WHERE sent_msg_id = ?1",
            [msg(2)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("m2 sent");
    assert_eq!((session.as_str(), agent.as_deref()), (R1, Some(S1)));
    let (session, name): (String, String) = conn
        .query_row(
            "SELECT session_id, peer_name FROM events WHERE peer_msg_id = ?1",
            [msg(3)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("m3 received");
    assert_eq!(
        (session.as_str(), name.as_str()),
        (R1, "ticket-demo-repo-1")
    );
}

#[test]
fn any_ingest_order_converges_to_the_cold_build() {
    let cold = Temp::new("cold");
    let want = spawns(&build(&cold, whole_files(&transcripts())));
    assert_eq!(want, self::want());

    let mut reversed = transcripts();
    reversed.reverse();
    let t = Temp::new("reversed");
    assert_eq!(spawns(&build(&t, whole_files(&reversed))), want, "reversed");

    // One line per batch, round-robin across files, the last file first.
    let files: Vec<Vec<TailEvent>> = reversed.iter().map(|p| records(p)).collect();
    let longest = files.iter().map(Vec::len).max().unwrap_or(0);
    let lines = (0..longest).flat_map(|i| {
        files
            .iter()
            .filter_map(move |f| f.get(i).map(|e| batch(vec![e.clone()])))
    });
    let t = Temp::new("incremental");
    let conn = build(&t, lines);
    assert_eq!(spawns(&conn), want, "incremental");
    drop(conn);

    // Re-ingesting everything moves nothing.
    let mut store = Store::open_in(t.db(), "UTC").expect("reopen");
    for b in whole_files(&transcripts()) {
        store.ingest(&b).expect("re-ingest");
    }
    drop(store);
    assert_eq!(
        spawns(&Connection::open(t.db()).expect("db")),
        want,
        "re-ingest"
    );
}

#[test]
fn opening_a_store_from_an_older_rollup_version_rereads_messages_once() {
    let t = Temp::new("reread");
    let conn = build(&t, whole_files(&transcripts()));
    conn.execute_batch(
        "UPDATE events SET sent_msg_id = NULL, peer_msg_id = NULL, peer_name = NULL;
         DELETE FROM edges WHERE kind = 'message';
         UPDATE meta SET value = '3' WHERE key = 'rollup';",
    )
    .expect("a store accounted by the build before");
    drop(conn);
    drop(Store::open_in(t.db(), "UTC").expect("reopen"));
    let conn = Connection::open(t.db()).expect("db");
    assert_eq!(spawns(&conn), want());
    let peers: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE peer_name IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("peers");
    assert_eq!(peers, 9);

    // At the current version, open rebuilds nothing.
    conn.execute("DELETE FROM edges WHERE kind = 'message'", [])
        .expect("marker");
    drop(conn);
    drop(Store::open_in(t.db(), "UTC").expect("reopen"));
    assert!(spawns(&Connection::open(t.db()).expect("db")).is_empty());
}

// ---- views ----------------------------------------------------------------

/// The cold build at `t`, kept open for registry polls, and a reader.
fn viewed(t: &Temp) -> (Store, Connection) {
    drop(build(t, whole_files(&transcripts())));
    let store = Store::open_in(t.db(), "UTC").expect("open");
    let reader = Store::open_reader(t.db()).expect("reader");
    (store, reader)
}

/// A registry entry for `session`, interactive under pid `pid`, with
/// `status` (and `waitingFor`, when waiting).
fn listed(pid: u32, session: &str, name: &str, status: &str) -> RegistryEvent {
    let waiting = if status == "waiting" {
        r#","waitingFor":"permission""#
    } else {
        ""
    };
    let json = format!(
        r#"[{{"pid":{pid},"cwd":"/work/hub","kind":"interactive","startedAt":1791000000000,
             "sessionId":"{session}","name":"{name}","status":"{status}"{waiting}}}]"#
    );
    RegistryEvent::Changed {
        entry: parse_snapshot(json.as_bytes()).expect("entry").remove(0),
        at_ms: 5000,
    }
}

fn gone(pid: u32, session: &str, name: &str) -> RegistryEvent {
    let RegistryEvent::Changed { entry, .. } = listed(pid, session, name, "busy") else {
        unreachable!()
    };
    RegistryEvent::Gone {
        entry,
        last_seen_ms: 6000,
    }
}

/// The tab labelled `label` (see [`TABS`]).
fn tab<'a>(reader: &Connection, rows: &'a [Workstream], label: &str) -> &'a Workstream {
    let id = tab_id(reader, label);
    rows.iter()
        .find(|w| w.id == id)
        .unwrap_or_else(|| panic!("no tab {label}: {rows:?}"))
}

/// A tab's id: its first link's orchestrator's row id.
fn tab_id(reader: &Connection, label: &str) -> i64 {
    let (_, session) = TABS.iter().find(|(l, _)| *l == label).expect("a label");
    reader
        .query_row(
            "SELECT id FROM agents WHERE session_id = ?1 AND agent_id IS NULL",
            [session],
            |r| r.get(0),
        )
        .expect("orchestrator")
}

/// Each tab's director, by label, in [`TABS`] order.
fn nesting(reader: &Connection) -> Vec<(&'static str, Option<&'static str>)> {
    let rows = views::workstreams(reader).expect("workstreams");
    let label = |id: i64| {
        TABS.iter()
            .map(|(l, _)| *l)
            .find(|l| tab_id(reader, l) == id)
            .unwrap_or_else(|| panic!("an unlabelled tab {id}: {rows:?}"))
    };
    assert_eq!(rows.len(), TABS.len(), "one tab per chain: {rows:?}");
    let mut out: Vec<_> = rows
        .iter()
        .map(|w| (label(w.id), w.director.map(label)))
        .collect();
    out.sort_by_key(|(l, _)| TABS.iter().position(|(t, _)| t == l));
    out
}

#[test]
fn a_clear_chain_is_one_tab_named_by_its_drawn_links_charge() {
    let t = Temp::new("one-chain");
    let (_store, reader) = viewed(&t);
    let rows = views::workstreams(&reader).expect("workstreams");
    let r = tab(&reader, &rows, "R");
    // Nothing is live, so R draws its latest link, R2.
    assert_eq!(
        (r.name.as_str(), r.fallback, r.session_id.as_deref()),
        ("topic_demo", false, Some(R2))
    );
    // N2 and N3 share a name but are two processes: two tabs.
    for label in ["N2", "N3"] {
        assert_eq!(tab(&reader, &rows, label).name, "issue_demo-repo-2");
    }
    assert_eq!(tab(&reader, &rows, "Q1").name, "topic_notes");
}

#[test]
fn a_spawned_tab_nests_under_its_spawners_tab_only_while_that_one_is_live() {
    let t = Temp::new("nesting");
    let (mut store, reader) = viewed(&t);
    let top_level: Vec<(&str, Option<&str>)> = TABS.iter().map(|(l, _)| (*l, None)).collect();
    // Nothing live: every chain keeps its own tab.
    assert_eq!(nesting(&reader), top_level);

    store
        .apply_registry(&[
            listed(4101, R2, "director-demo", "busy"),
            listed(4102, X1, "director-alpha", "busy"),
            listed(4103, X2, "director-beta", "busy"),
            listed(4106, Q1, "general-notes", "busy"),
        ])
        .expect("spawners live");
    // N2 nests by its spawner, R1's subagent; Q3 under general-notes, no
    // director's name needed. X2 nests under X1 (its message back was
    // refused, so not the other way), so N4, X2's, keeps its own tab.
    assert_eq!(
        nesting(&reader),
        vec![
            ("R", None),
            ("N1", Some("R")),
            ("N2", Some("R")),
            ("N3", None),
            ("Q1", None),
            ("Q3", Some("Q1")),
            ("X1", None),
            ("X2", Some("X1")),
            ("N4", None),
        ]
    );

    store
        .apply_registry(&[gone(4101, R2, "director-demo")])
        .expect("spawner gone");
    let now = nesting(&reader);
    assert!(now.contains(&("N1", None)), "{now:?}");
    assert!(now.contains(&("N2", None)), "{now:?}");
}

/// Every live tab is top-level, or nests under a live top-level tab.
fn every_live_session_is_visible(rows: &[Workstream]) {
    for w in rows.iter().filter(|w| w.live) {
        if let Some(d) = w.director {
            let director = rows.iter().find(|x| x.id == d).expect("director");
            assert!(director.live && director.director.is_none(), "{w:?}");
        }
    }
}

#[test]
fn a_unit_of_a_nested_unit_keeps_its_own_tab() {
    let t = Temp::new("grandchild");
    let (mut store, reader) = viewed(&t);
    let chain = |reader: &Connection| -> Vec<(&str, Option<&str>)> {
        let rows = views::workstreams(reader).expect("workstreams");
        every_live_session_is_visible(&rows);
        nesting(reader)
            .into_iter()
            .filter(|(l, _)| ["X1", "X2", "N4"].contains(l))
            .collect()
    };
    // X1 -> X2 -> N4, all live.
    store
        .apply_registry(&[
            listed(4102, X1, "director-alpha", "busy"),
            listed(4103, X2, "director-beta", "idle"),
            listed(4105, N4, "ticket-demo-repo-4", "waiting"),
        ])
        .expect("live");
    assert_eq!(
        chain(&reader),
        vec![("X1", None), ("X2", Some("X1")), ("N4", None)]
    );
    // Folding is one level: N4's wait shows on its own tab, not on X1's.
    let rows = views::workstreams(&reader).expect("workstreams");
    let needs = |label: &str| {
        let w = tab(&reader, &rows, label);
        (w.needs_you, w.state)
    };
    assert_eq!(needs("N4"), (1, AgentState::NeedsYou));
    assert_eq!(needs("X2"), (0, AgentState::Idle));
    assert_eq!(needs("X1"), (0, AgentState::Working));

    // X2 waiting shows on X1's tab.
    store
        .apply_registry(&[listed(4103, X2, "director-beta", "waiting")])
        .expect("X2 waits");
    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(tab(&reader, &rows, "X1").needs_you, 1);

    // With X1 gone, X2 is a tab and N4 nests under it.
    store
        .apply_registry(&[gone(4102, X1, "director-alpha")])
        .expect("X1 gone");
    assert_eq!(
        chain(&reader),
        vec![("X1", None), ("X2", None), ("N4", Some("X2"))]
    );
}

#[test]
fn a_spawners_tab_counts_what_its_nested_units_need() {
    let t = Temp::new("needs");
    let (mut store, reader) = viewed(&t);
    let spawner = |reader: &Connection| -> (AgentState, i64) {
        let rows = views::workstreams(reader).expect("workstreams");
        let w = tab(reader, &rows, "R");
        (w.state, w.needs_you)
    };
    store
        .apply_registry(&[
            listed(4101, R2, "director-demo", "idle"),
            listed(4104, N1, "ticket-demo-repo-1", "waiting"),
        ])
        .expect("unit waits");
    assert_eq!(spawner(&reader), (AgentState::NeedsYou, 1));
    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(tab(&reader, &rows, "N1").needs_you, 1);
    let needs = views::needs_you(&reader).expect("needs_you");
    assert_eq!(
        needs.iter().map(|n| n.workstream).collect::<Vec<_>>(),
        [Some(tab_id(&reader, "N1"))],
        "a wait names its own chain's tab"
    );

    store
        .apply_registry(&[listed(4104, N1, "ticket-demo-repo-1", "busy")])
        .expect("unit works");
    assert_eq!(spawner(&reader), (AgentState::Working, 0));

    store
        .apply_registry(&[listed(4104, N1, "ticket-demo-repo-1", "idle")])
        .expect("unit idles");
    assert_eq!(spawner(&reader), (AgentState::Idle, 0));
}

#[test]
fn the_spawners_canvas_draws_every_live_session_any_of_its_links_spawned() {
    let t = Temp::new("canvas");
    let (mut store, reader) = viewed(&t);
    store
        .apply_registry(&[
            listed(4101, R2, "director-demo", "busy"),
            listed(4104, N1, "ticket-demo-repo-1", "busy"),
            listed(4107, N2, "ticket-demo-repo-2", "busy"),
            listed(4103, X2, "director-beta", "busy"),
        ])
        .expect("live");
    let rows = views::workstreams(&reader).expect("workstreams");
    let graph = views::live_graph(&reader, tab_id(&reader, "R"))
        .expect("query")
        .expect("a graph");
    // R2 is drawn; R1 and its subagent S1 spawned N1 and N2. S1 (an
    // earlier link's) and N1's subagent are done, so neither is drawn
    // (#81), and both units hang under the drawn orchestrator.
    assert_eq!(graph.session_id, R2);
    let drawn: Vec<(&str, Option<&str>, &str, AgentState)> = graph
        .agents
        .iter()
        .map(|a| {
            (
                a.kind.as_str(),
                a.agent_id.as_deref(),
                a.name.as_deref().unwrap_or("-"),
                a.state,
            )
        })
        .collect();
    assert_eq!(
        drawn,
        [
            ("orchestrator", None, "director-demo", AgentState::Working),
            (
                "orchestrator",
                None,
                "ticket-demo-repo-1",
                AgentState::Working
            ),
            (
                "orchestrator",
                None,
                "ticket-demo-repo-2",
                AgentState::Working
            ),
        ]
    );
    let id = |i: usize| graph.agents[i].id;
    let (spawner, n1, n2) = (id(0), id(1), id(2));
    assert_eq!(
        graph.agents.iter().map(|a| a.parent).collect::<Vec<_>>(),
        [None, Some(spawner), Some(spawner)]
    );
    let edge = |kind: &str, parent, child| GraphEdge {
        kind: kind.into(),
        parent,
        child,
    };
    assert_eq!(
        graph.edges,
        [edge("directs", spawner, n1), edge("directs", spawner, n2)]
    );

    // A live unit that spawns in turn: X1's canvas draws X2 under it.
    let graph = views::live_graph(&reader, tab(&reader, &rows, "X1").id)
        .expect("query")
        .expect("a graph");
    assert_eq!(graph.agents.len(), 2);
    assert_eq!(
        graph.edges,
        [edge("directs", graph.agents[0].id, graph.agents[1].id)]
    );
    // A session that spawned nothing draws no units.
    let graph = views::live_graph(&reader, tab_id(&reader, "Q3"))
        .expect("query")
        .expect("a graph");
    assert!(graph.edges.is_empty());
}
