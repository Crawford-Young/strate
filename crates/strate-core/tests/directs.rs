//! Director-launched units (#53) over the synthetic
//! `tests/fixtures/directs-home`: the msg_id a session sent and the peer
//! msg_id and name it received, and the `directs` edges rebuilt from them,
//! whatever order the transcripts arrive in; and the views over them: unit
//! tabs nested under a live director's, and the units on its canvas.
//! Every db lives in a temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{self, AgentState, GraphEdge, Workstream};
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

/// director-demo.
const R1: &str = "5e55d7d7-0000-4000-8000-0000000000c1";
/// director-demo after /clear: R1's successor, drawn while live.
const R2: &str = "5e55d7d7-0000-4000-8000-0000000000c0";
/// ticket-demo-repo-1, messaged by R1 first, then by R2.
const N1: &str = "5e55d7d7-0000-4000-8000-0000000000e1";
/// ticket-demo-repo-2, messaged from R1's subagent; never replies.
const N2: &str = "5e55d7d7-0000-4000-8000-0000000000e2";
/// director-alpha, messaged back by X2 (a cycle).
const X1: &str = "5e55d7d7-0000-4000-8000-0000000000b1";
/// director-beta: directed by X1, directs N4.
const X2: &str = "5e55d7d7-0000-4000-8000-0000000000b2";
/// ticket-demo-repo-4, directed by X2.
const N4: &str = "5e55d7d7-0000-4000-8000-0000000000e4";
/// R1's subagent.
const S1: &str = "a7000000000000001";
/// N1's subagent.
const S2: &str = "a7000000000000002";

fn msg(n: u8) -> String {
    format!("5e55ee00-0000-4000-8000-{n:012x}")
}

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("strate-directs-{name}-{}", std::process::id()));
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/directs-home")
}

/// Every transcript under `root/projects`, sorted.
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

/// One record event per line of `path`, with its byte offset.
fn records(path: &Path) -> Vec<TailEvent> {
    let text = fs::read_to_string(path).expect("read");
    let path: Arc<Path> = path.into();
    let mut offset = 0u64;
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        out.push(TailEvent::Record {
            path: path.clone(),
            offset,
            value: serde_json::from_str(line).expect("fixture line is json"),
        });
        offset += line.len() as u64;
    }
    out
}

/// `events` as one batch, checkpointed past its last record.
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

/// (parent, child, msg_id) per stored edge, by child.
fn directs(conn: &Connection) -> Vec<(String, String, String)> {
    conn.prepare("SELECT parent, child, msg_id FROM directs ORDER BY child")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(|e| e.expect("edge"))
        .collect()
}

fn edge(parent: &str, child: &str, n: u8) -> (String, String, String) {
    (parent.into(), child.into(), msg(n))
}

/// The edges the fixture encodes, by child id.
fn want() -> Vec<(String, String, String)> {
    vec![
        edge(X1, X2, 8),
        edge(R1, N1, 1),
        edge(R1, N2, 2),
        edge(X2, N4, 10),
    ]
}

#[test]
fn directors_parent_the_sessions_they_messaged_first() {
    let t = Temp::new("edges");
    let conn = build(&t, whole_files(&transcripts()));
    // Not edges: N1's reply (no director prefix), R1's message to a session
    // not on disk, R2's later message to N1, the general-notes pair, and
    // X2's message back to X1 (a cycle).
    assert_eq!(directs(&conn), want());
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
    let want = directs(&build(&cold, whole_files(&transcripts())));
    assert_eq!(want, self::want());

    let mut reversed = transcripts();
    reversed.reverse();
    let t = Temp::new("reversed");
    assert_eq!(
        directs(&build(&t, whole_files(&reversed))),
        want,
        "reversed"
    );

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
    assert_eq!(directs(&conn), want, "incremental");
    drop(conn);

    // Re-ingesting everything moves nothing.
    let mut store = Store::open_in(t.db(), "UTC").expect("reopen");
    for b in whole_files(&transcripts()) {
        store.ingest(&b).expect("re-ingest");
    }
    drop(store);
    assert_eq!(
        directs(&Connection::open(t.db()).expect("db")),
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
         DELETE FROM directs;
         UPDATE meta SET value = '3' WHERE key = 'rollup';",
    )
    .expect("a store accounted by the build before");
    drop(conn);
    drop(Store::open_in(t.db(), "UTC").expect("reopen"));
    let conn = Connection::open(t.db()).expect("db");
    assert_eq!(directs(&conn), want());
    let peers: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE peer_name IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("peers");
    assert_eq!(peers, 9);

    // At the current version, open rebuilds nothing.
    conn.execute("DELETE FROM directs", []).expect("marker");
    drop(conn);
    drop(Store::open_in(t.db(), "UTC").expect("reopen"));
    assert!(directs(&Connection::open(t.db()).expect("db")).is_empty());
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

fn tab<'a>(rows: &'a [Workstream], name: &str) -> &'a Workstream {
    rows.iter()
        .find(|w| w.name == name)
        .unwrap_or_else(|| panic!("no workstream {name}: {rows:?}"))
}

/// Each tab's director, by name.
fn nesting(reader: &Connection) -> Vec<(String, Option<String>)> {
    let rows = views::workstreams(reader).expect("workstreams");
    let mut out: Vec<(String, Option<String>)> = rows
        .iter()
        .map(|w| {
            let director = w
                .director
                .map(|d| rows.iter().find(|x| x.id == d).expect("a tab").name.clone());
            (w.name.clone(), director)
        })
        .collect();
    out.sort();
    out
}

fn nested(name: &str, director: Option<&str>) -> (String, Option<String>) {
    (name.into(), director.map(str::to_string))
}

#[test]
fn both_director_demo_sessions_group_into_one_workstream() {
    let t = Temp::new("one-director");
    let (_store, reader) = viewed(&t);
    let roots: Vec<(String, String)> = reader
        .prepare(
            "SELECT DISTINCT g.session_id, w.name FROM segments AS g
             JOIN workstream_roots AS r ON r.id = g.workstream_id
             JOIN workstreams AS w ON w.id = r.root
             WHERE g.session_id IN (?1, ?2) ORDER BY g.session_id",
        )
        .expect("prepare")
        .query_map([R1, R2], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(
        roots,
        [
            (R2.to_string(), "director-demo".to_string()),
            (R1.to_string(), "director-demo".to_string())
        ]
    );
}

#[test]
fn a_unit_tab_nests_under_its_director_only_while_the_director_is_live() {
    let t = Temp::new("nesting");
    let (mut store, reader) = viewed(&t);
    let top_level = vec![
        nested("director-alpha", None),
        nested("director-beta", None),
        nested("director-demo", None),
        nested("general-notes", None),
        nested("ticket-demo-repo-1", None),
        nested("ticket-demo-repo-2", None),
        nested("ticket-demo-repo-3", None),
        nested("ticket-demo-repo-4", None),
    ];
    // Nothing live: every workstream keeps its own tab.
    assert_eq!(nesting(&reader), top_level);

    store
        .apply_registry(&[
            listed(4101, R2, "director-demo", "busy"),
            listed(4102, X1, "director-alpha", "busy"),
            listed(4103, X2, "director-beta", "busy"),
        ])
        .expect("directors live");
    // ticket-demo-repo-2 draws N3, which nobody directed: it keeps its tab
    // though N2 (directed by R1) is in it. director-beta nests under
    // director-alpha; the reverse edge was refused, so not the other way.
    assert_eq!(
        nesting(&reader),
        vec![
            nested("director-alpha", None),
            nested("director-beta", Some("director-alpha")),
            nested("director-demo", None),
            nested("general-notes", None),
            nested("ticket-demo-repo-1", Some("director-demo")),
            nested("ticket-demo-repo-2", None),
            nested("ticket-demo-repo-3", None),
            // Its director, director-beta, is itself nested.
            nested("ticket-demo-repo-4", None),
        ]
    );

    let RegistryEvent::Changed { entry, .. } = listed(4101, R2, "director-demo", "busy") else {
        unreachable!()
    };
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry,
            last_seen_ms: 6000,
        }])
        .expect("director gone");
    let now = nesting(&reader);
    assert_eq!(
        now.iter()
            .find(|(n, _)| n == "ticket-demo-repo-1")
            .expect("tab"),
        &nested("ticket-demo-repo-1", None)
    );
}

/// Every live workstream is a tab, or nests under a live tab that is one.
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
    let chain = |reader: &Connection| -> Vec<(String, Option<String>)> {
        let rows = views::workstreams(reader).expect("workstreams");
        every_live_session_is_visible(&rows);
        nesting(reader)
            .into_iter()
            .filter(|(n, _)| {
                ["director-alpha", "director-beta", "ticket-demo-repo-4"].contains(&n.as_str())
            })
            .collect()
    };
    // director-alpha -> director-beta -> ticket-demo-repo-4, all live.
    store
        .apply_registry(&[
            listed(4102, X1, "director-alpha", "busy"),
            listed(4103, X2, "director-beta", "idle"),
            listed(4105, N4, "ticket-demo-repo-4", "waiting"),
        ])
        .expect("live");
    assert_eq!(
        chain(&reader),
        vec![
            nested("director-alpha", None),
            nested("director-beta", Some("director-alpha")),
            nested("ticket-demo-repo-4", None),
        ]
    );
    // Folding is one level: N4's wait shows on its own tab, not on alpha's.
    let rows = views::workstreams(&reader).expect("workstreams");
    let needs = |name: &str| (tab(&rows, name).needs_you, tab(&rows, name).state);
    assert_eq!(needs("ticket-demo-repo-4"), (1, AgentState::NeedsYou));
    assert_eq!(needs("director-beta"), (0, AgentState::Idle));
    assert_eq!(needs("director-alpha"), (0, AgentState::Working));

    // director-beta waiting shows on alpha's tab.
    store
        .apply_registry(&[listed(4103, X2, "director-beta", "waiting")])
        .expect("beta waits");
    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(tab(&rows, "director-alpha").needs_you, 1);

    // With director-alpha gone, director-beta is a tab and N4 nests under it.
    let RegistryEvent::Changed { entry, .. } = listed(4102, X1, "director-alpha", "busy") else {
        unreachable!()
    };
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry,
            last_seen_ms: 6000,
        }])
        .expect("alpha gone");
    assert_eq!(
        chain(&reader),
        vec![
            nested("director-alpha", None),
            nested("director-beta", None),
            nested("ticket-demo-repo-4", Some("director-beta")),
        ]
    );
}

#[test]
fn a_director_tab_counts_what_its_nested_units_need() {
    let t = Temp::new("needs");
    let (mut store, reader) = viewed(&t);
    let director = |reader: &Connection| -> (AgentState, i64) {
        let rows = views::workstreams(reader).expect("workstreams");
        let w = tab(&rows, "director-demo");
        (w.state, w.needs_you)
    };
    store
        .apply_registry(&[
            listed(4101, R2, "director-demo", "idle"),
            listed(4104, N1, "ticket-demo-repo-1", "waiting"),
        ])
        .expect("unit waits");
    assert_eq!(director(&reader), (AgentState::NeedsYou, 1));
    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(tab(&rows, "ticket-demo-repo-1").needs_you, 1);

    store
        .apply_registry(&[listed(4104, N1, "ticket-demo-repo-1", "busy")])
        .expect("unit works");
    assert_eq!(director(&reader), (AgentState::Working, 0));

    store
        .apply_registry(&[listed(4104, N1, "ticket-demo-repo-1", "idle")])
        .expect("unit idles");
    assert_eq!(director(&reader), (AgentState::Idle, 0));
}

#[test]
fn the_director_canvas_draws_every_unit_any_of_its_sessions_directed() {
    let t = Temp::new("canvas");
    let (mut store, reader) = viewed(&t);
    store
        .apply_registry(&[
            listed(4101, R2, "director-demo", "busy"),
            listed(4104, N1, "ticket-demo-repo-1", "busy"),
        ])
        .expect("live");
    let rows = views::workstreams(&reader).expect("workstreams");
    let graph = views::live_graph(&reader, tab(&rows, "director-demo").id)
        .expect("query")
        .expect("a graph");
    // R2 is drawn; R1 directed both units. R1's own subagent S1 is not drawn.
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
            ("subagent", Some(S2), "-", AgentState::Done),
            // N2 is not live: a done unit.
            ("orchestrator", None, "ticket-demo-repo-2", AgentState::Done),
        ]
    );
    assert!(
        !graph
            .agents
            .iter()
            .any(|a| a.agent_id.as_deref() == Some(S1))
    );
    let id = |i: usize| graph.agents[i].id;
    let (director, n1, s2, n2) = (id(0), id(1), id(2), id(3));
    assert_eq!(
        graph.agents.iter().map(|a| a.parent).collect::<Vec<_>>(),
        [None, Some(director), Some(n1), Some(director)]
    );
    let edge = |kind: &str, parent, child| GraphEdge {
        kind: kind.into(),
        parent,
        child,
    };
    assert_eq!(
        graph.edges,
        [
            edge("directs", director, n1),
            edge("dispatch", n1, s2),
            edge("directs", director, n2),
        ]
    );

    // A unit that is itself a director: alpha's canvas draws beta.
    let graph = views::live_graph(&reader, tab(&rows, "director-alpha").id)
        .expect("query")
        .expect("a graph");
    assert_eq!(graph.agents.len(), 2);
    assert_eq!(
        graph.edges,
        [edge("directs", graph.agents[0].id, graph.agents[1].id)]
    );
    // An undirected session draws no units.
    let graph = views::live_graph(&reader, tab(&rows, "ticket-demo-repo-3").id)
        .expect("query")
        .expect("a graph");
    assert!(graph.edges.is_empty());
}
