//! A `/clear` chain renamed across its links and messaged by a younger
//! session (#133), over the synthetic `tests/fixtures/peers-home`: a
//! messager whose chain started after the recipient's chain is a peer, not
//! its spawner, so the chain keeps its own tab; and the chain draws as one
//! tab and one node under its latest link's name and charge, the node
//! naming every link of its chain. Every db lives in a temp dir.

use std::path::PathBuf;

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{self, Workstream};

mod common;

use common::{Temp, batch, fixture_home, transcripts};

/// director_topic_lorem-11-19: the chain's first link, from 2026-10-21.
const D1: &str = "5e55f3f3-0000-4000-8000-0000000000a1";
/// D1's `/clear` successor.
const D2: &str = "5e55f3f3-0000-4000-8000-0000000000a2";
/// D2's successor, renamed to director_topic_lorem-21-29.
const D3: &str = "5e55f3f3-0000-4000-8000-0000000000a3";
/// Unnamed, started 2026-10-23: messages D1, a peer.
const Y: &str = "5e55f3f3-0000-4000-8000-0000000000b1";
/// director_topic_amet, the first link of its chain; sends no message.
const O: &str = "5e55f3f3-0000-4000-8000-0000000000c1";
/// lorem-w, started after O but before O2, messaged by O2.
const W: &str = "5e55f3f3-0000-4000-8000-0000000000c3";
/// O's `/clear` successor.
const O2: &str = "5e55f3f3-0000-4000-8000-0000000000c4";

/// A build of peers-home at `t`, its transcripts ingested whole in
/// `order`, kept open for registry polls, and a reader.
fn built_in(t: &Temp, order: &[PathBuf]) -> (Store, Connection) {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&fixture_home("peers-home")).expect("discover"))
        .expect("graph");
    for path in order {
        store.ingest(&batch(path)).expect("ingest");
    }
    let reader = Store::open_reader(t.db()).expect("reader");
    (store, reader)
}

/// The cold build: every transcript, in path order.
fn built(t: &Temp) -> (Store, Connection) {
    built_in(t, &transcripts(&fixture_home("peers-home")))
}

/// The registry listing `session` live and interactive, under pid `pid`.
fn listed(pid: u32, session: &str, name: &str, status: &str) -> RegistryEvent {
    let json = format!(
        r#"[{{"pid":{pid},"cwd":"/work/hub","kind":"interactive","startedAt":1792000000000,
             "sessionId":"{session}","name":"{name}","status":"{status}"}}]"#
    );
    RegistryEvent::Changed {
        entry: parse_snapshot(json.as_bytes()).expect("entry").remove(0),
        at_ms: 5000,
    }
}

/// `session`'s orchestrator's row id.
fn orchestrator(reader: &Connection, session: &str) -> i64 {
    reader
        .query_row(
            "SELECT id FROM agents WHERE session_id = ?1 AND agent_id IS NULL",
            [session],
            |r| r.get(0),
        )
        .expect("orchestrator")
}

/// Every `message` edge as (spawner session, spawned session), by spawned.
fn spawns(reader: &Connection) -> Vec<(String, String)> {
    reader
        .prepare(
            "SELECT p.session_id, c.session_id FROM edges AS e
             JOIN agents AS p ON p.id = e.parent JOIN agents AS c ON c.id = e.child
             WHERE e.kind = 'message' ORDER BY c.session_id",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|e| e.expect("edge"))
        .collect()
}

/// The tab whose first link is `session`.
fn tab<'a>(reader: &Connection, rows: &'a [Workstream], session: &str) -> &'a Workstream {
    let id = orchestrator(reader, session);
    rows.iter()
        .find(|w| w.id == id)
        .unwrap_or_else(|| panic!("no tab {session}: {rows:?}"))
}

#[test]
fn a_messager_younger_than_the_recipients_chain_is_a_peer_not_its_spawner() {
    let t = Temp::new("peers-edges");
    let (_store, reader) = built(&t);
    // Y messaged D1 three days into D1's chain: no edge. O's chain is older
    // than W, so W hangs under O2, a link of that chain, though O2 itself
    // started after W.
    assert_eq!(spawns(&reader), [(O2.to_string(), W.to_string())]);
}

#[test]
fn a_chain_aging_as_its_earlier_links_arrive_converges_to_the_cold_build() {
    let cold = Temp::new("peers-cold");
    let want = spawns(&built(&cold).1);
    // O2 and W first: until O arrives, O2's chain is younger than W. O's
    // transcript holds no message, only the start that ages its chain.
    let file = |s: &str| {
        transcripts(&fixture_home("peers-home"))
            .into_iter()
            .find(|p| p.to_string_lossy().contains(s))
            .expect("a transcript")
    };
    let mut order: Vec<PathBuf> = [O2, W, Y, D3, D2, D1, O].map(file).to_vec();
    let t = Temp::new("peers-late-root");
    assert_eq!(spawns(&built_in(&t, &order).1), want, "earliest link last");
    order.reverse();
    let t = Temp::new("peers-reversed");
    assert_eq!(spawns(&built_in(&t, &order).1), want, "reversed");
}

#[test]
fn a_chain_messaged_by_a_younger_session_keeps_its_own_tab_under_its_latest_name() {
    let t = Temp::new("peers-tabs");
    let (mut store, reader) = built(&t);
    store
        .apply_registry(&[
            listed(4201, D3, "director_topic_lorem-21-29", "busy"),
            listed(4202, Y, "code-lorem", "idle"),
        ])
        .expect("both live");
    let rows = views::workstreams(&reader).expect("workstreams");
    // One tab for the three links, drawn and named by the latest.
    assert_eq!(
        rows.iter()
            .filter(|w| [D1, D2, D3].contains(&w.session_id.as_deref().unwrap_or("")))
            .count(),
        1,
        "{rows:?}"
    );
    let d = tab(&reader, &rows, D1);
    assert_eq!(
        (d.name.as_str(), d.session_id.as_deref(), d.live, d.director),
        ("topic_lorem-21-29", Some(D3), true, None),
        "a top-level tab of its own"
    );
    let y = tab(&reader, &rows, Y);
    assert_eq!((y.live, y.director), (true, None));

    // Neither canvas draws the other.
    let graph = views::live_graph(&reader, orchestrator(&reader, Y))
        .expect("query")
        .expect("a graph");
    let sessions: Vec<&str> = graph.agents.iter().map(|a| a.session_id.as_str()).collect();
    assert_eq!(sessions, [Y]);
}

#[test]
fn a_chains_node_is_its_latest_link_naming_every_link_of_the_chain() {
    let t = Temp::new("peers-node");
    let (mut store, reader) = built(&t);
    store
        .apply_registry(&[listed(4201, D3, "director_topic_lorem-21-29", "busy")])
        .expect("live");
    let links = [D1, D2, D3].map(|s| orchestrator(&reader, s));
    // Any link's orchestrator id reads as the chain's tab.
    for link in links {
        let graph = views::live_graph(&reader, link)
            .expect("query")
            .expect("a graph");
        assert_eq!(graph.session_id, D3);
        let node = &graph.agents[0];
        assert_eq!(
            (
                node.id,
                node.name.as_deref(),
                node.role.as_deref(),
                node.charge_key.as_deref(),
            ),
            (
                links[2],
                Some("director_topic_lorem-21-29"),
                Some("director"),
                Some("topic_lorem-21-29"),
            )
        );
        // What the console follows when an earlier link was open (#133).
        assert_eq!(node.links, links);
    }
}

#[test]
fn a_renamed_link_is_named_by_its_own_latest_title_before_the_registry_lists_it() {
    let t = Temp::new("peers-unlisted");
    let (_store, reader) = built(&t);
    // Nothing live: the latest link draws, under its own rename.
    let graph = views::live_graph(&reader, orchestrator(&reader, D1))
        .expect("query")
        .expect("a graph");
    let node = &graph.agents[0];
    assert_eq!(
        (node.session_id.as_str(), node.name.as_deref()),
        (D3, Some("director_topic_lorem-21-29"))
    );
}
