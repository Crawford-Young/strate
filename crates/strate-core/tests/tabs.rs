//! Tabs: `/clear` chains (#13, one tab per chain since #107) over the
//! synthetic fixture in `tests/fixtures/workstreams-home`: each session's
//! start and process root, the continuation edges linking a root's
//! sessions, one tab per chain named by its latest link's charge
//! (`views::workstreams`), and incremental ingest converging to a cold
//! build. Every db lives in a temp dir.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::store::views;
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

const C1: &str = "5e55c0c0-0000-4000-8000-0000000000c1";
const C9: &str = "5e55c0c0-0000-4000-8000-0000000000c9";
const C5: &str = "5e55c0c0-0000-4000-8000-0000000000c5";
const D1: &str = "5e55d0d0-0000-4000-8000-0000000000d1";
const D2: &str = "5e55d0d0-0000-4000-8000-0000000000d2";
const E1: &str = "5e55e0e0-0000-4000-8000-0000000000e1";
const E2: &str = "5e55e0e0-0000-4000-8000-0000000000e2";
/// E1's and E2's latest cwd and branch.
const PLACE: &str = "/work/demo-repo/packages/core @ feat/9-lorem";

mod common;

use common::{Temp, fixture_home, records as records_in, transcripts as transcripts_in};

fn root() -> PathBuf {
    fixture_home("workstreams-home")
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

/// Cold build: the discovery graph, then each transcript as one batch.
fn cold(store: &mut Store) {
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    for path in transcripts() {
        store.ingest(&batch(records(&path))).expect("ingest");
    }
}

fn built(name: &str) -> (Temp, Store, Connection) {
    let tmp = Temp::new(name);
    let mut store = Store::open(tmp.db()).expect("open");
    cold(&mut store);
    let conn = Store::open_reader(tmp.db()).expect("reader");
    (tmp, store, conn)
}

/// The session whose orchestrator is row `id`.
fn session_of(conn: &Connection, id: i64) -> String {
    conn.query_row("SELECT session_id FROM agents WHERE id = ?1", [id], |r| {
        r.get(0)
    })
    .expect("orchestrator")
}

/// Each tab, keyed by sessions (row ids differ between builds): its first
/// link, drawn link, name, whether it is Unlinked, start and $.
fn tabs(conn: &Connection) -> BTreeSet<String> {
    views::workstreams(conn)
        .expect("workstreams")
        .into_iter()
        .map(|w| {
            format!(
                "tab {} {} {} {} {} {:.6}",
                session_of(conn, w.id),
                w.session_id.unwrap_or_default(),
                w.name,
                w.fallback,
                w.started_at.unwrap_or_default(),
                w.cost_usd.unwrap_or_default()
            )
        })
        .collect()
}

/// Everything the chains and tabs read, keyed by session ids.
fn snapshot(conn: &Connection) -> BTreeSet<String> {
    let mut out = tabs(conn);
    let mut rows = |sql: &str| {
        let mut stmt = conn.prepare(sql).expect("prepare");
        let lines: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .expect("query")
            .map(|l| l.expect("row"))
            .collect();
        out.extend(lines);
    };
    rows(
        "SELECT 'session ' || session_id || ' ' || ifnull(started_at, '-') || ' '
             || ifnull(process_root, '-') FROM sessions",
    );
    rows(
        "SELECT 'edge ' || p.session_id || ' -> ' || c.session_id FROM edges AS e
         JOIN agents AS p ON p.id = e.parent JOIN agents AS c ON c.id = e.child
         WHERE e.kind = 'continuation'",
    );
    rows(
        "SELECT 'own ' || a.session_id || ' ' || ifnull(a.agent_id, '-') || ' '
             || ifnull(c.key, '-') || ' ' || ifnull(a.own_charge_source, '-')
         FROM agents AS a LEFT JOIN charges AS c ON c.id = a.own_charge",
    );
    out
}

#[test]
fn continuation_edges_link_a_process_roots_files_in_start_order() {
    let (_tmp, _store, conn) = built("continuation");
    let edges: BTreeSet<String> = snapshot(&conn)
        .into_iter()
        .filter(|l| l.starts_with("edge "))
        .collect();
    let want: BTreeSet<String> = [(C1, C9), (C9, C5), (D1, D2)]
        .iter()
        .map(|(p, c)| format!("edge {p} -> {c}"))
        .collect();
    assert_eq!(edges, want, "E1/E2 carry no session_id and get no edge");
}

#[test]
fn a_session_starts_at_its_orchestrators_earliest_record() {
    let (_tmp, _store, conn) = built("starts");
    let start = |s: &str| -> Option<String> {
        conn.query_row(
            "SELECT started_at FROM sessions WHERE session_id = ?1",
            [s],
            |r| r.get(0),
        )
        .expect("session")
    };
    // C9 opens with a carried title and a /clear: its first timestamped
    // record. A subagent's earlier record (none here) would not count.
    assert_eq!(start(C1).as_deref(), Some("2026-10-02T10:00:00.000Z"));
    assert_eq!(start(C9).as_deref(), Some("2026-10-02T11:00:00.000Z"));
    assert_eq!(start(E1).as_deref(), Some("2026-10-02T08:00:00.000Z"));
}

#[test]
fn a_clear_chain_is_one_tab_named_by_its_latest_links_charge() {
    let (_tmp, _store, conn) = built("tabs");
    let names: BTreeSet<String> = tabs(&conn)
        .into_iter()
        .map(|t| t.rsplit_once(' ').expect("$").0.to_string())
        .collect();
    let tab = |first: &str, drawn: &str, name: &str, unlinked: bool, start: &str| {
        format!("tab {first} {drawn} {name} {unlinked} 2026-10-02T{start}.000Z")
    };
    // C1 -> C9 -> C5, renamed demo-repo-81, -82 and -83 on the way: one
    // tab, C5's charge. D1 -> D2 likewise; E1 and E2 share a cwd but are
    // two processes, each Unlinked and named by where it ran.
    let want: BTreeSet<String> = [
        tab(C1, C5, "issue_demo-repo-83", false, "12:00:00"),
        tab(D1, D2, "issue_demo-repo-91", false, "09:30:00"),
        tab(E1, E1, PLACE, true, "08:00:00"),
        tab(E2, E2, PLACE, true, "08:30:00"),
    ]
    .into_iter()
    .collect();
    assert_eq!(names, want);
}

#[test]
fn incremental_ingest_converges_to_a_cold_build_and_re_ingest_is_idempotent() {
    let (_cold_tmp, mut cold_store, cold_conn) = built("cold");
    let want = snapshot(&cold_conn);
    assert!(want.iter().any(|l| l.starts_with("tab ")), "{want:?}");

    let tmp = Temp::new("incremental");
    let mut store = Store::open(tmp.db()).expect("open");
    let conn = Store::open_reader(tmp.db()).expect("reader");
    // Subagent files first, then one line per batch, round-robin across
    // files, the graph last.
    let mut files: Vec<Vec<TailEvent>> = transcripts().iter().map(|p| records(p)).collect();
    files.sort_by_key(|f| {
        let TailEvent::Record { path, .. } = &f[0] else {
            unreachable!()
        };
        !path.to_string_lossy().contains("subagents")
    });
    let mut round = 0;
    while files.iter().any(|f| round < f.len()) {
        for file in &files {
            if let Some(event) = file.get(round) {
                store.ingest(&batch(vec![event.clone()])).expect("ingest");
            }
        }
        round += 1;
    }
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    assert_eq!(snapshot(&conn), want, "incremental == cold");

    // Re-ingest and rebuild the model: nothing moves.
    cold(&mut cold_store);
    cold_store.rebuild_model().expect("rebuild");
    assert_eq!(snapshot(&cold_conn), want);
}
