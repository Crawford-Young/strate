//! Workstream grouping (#13) over the synthetic fixture in
//! `tests/fixtures/workstreams-home`: name segments split at each rename,
//! backfill, carried-name preludes merging forward, the cwd/branch fallback,
//! subagent attribution, continuation edges, incremental convergence and
//! manual merges. Every db lives in a temp dir.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, params};
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

const C1: &str = "5e55c0c0-0000-4000-8000-0000000000c1";
const C9: &str = "5e55c0c0-0000-4000-8000-0000000000c9";
const C5: &str = "5e55c0c0-0000-4000-8000-0000000000c5";
const D1: &str = "5e55d0d0-0000-4000-8000-0000000000d1";
const D2: &str = "5e55d0d0-0000-4000-8000-0000000000d2";
const E1: &str = "5e55e0e0-0000-4000-8000-0000000000e1";
const E2: &str = "5e55e0e0-0000-4000-8000-0000000000e2";
const FALLBACK: &str = "/work/demo-repo/packages/core @ feat/9-lorem";

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-ws-{name}-{}", std::process::id()));
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workstreams-home")
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

fn transcript(session: &str) -> PathBuf {
    transcripts()
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(&format!("{session}.jsonl")))
        .expect("session transcript")
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

fn offset_of(session: &str, needle: &str) -> i64 {
    let text = fs::read_to_string(transcript(session)).expect("read");
    text.find(needle).expect("needle in transcript") as i64
}

fn title_offset(session: &str, name: &str) -> i64 {
    offset_of(session, &format!("\"customTitle\":\"{name}\""))
        - "{\"type\":\"custom-title\",".len() as i64
}

/// (start_offset, workstream name, fallback) per segment of `session`.
fn segments(conn: &Connection, session: &str) -> Vec<(i64, String, bool)> {
    let mut stmt = conn
        .prepare(
            "SELECT s.start_offset, w.name, w.fallback FROM segments AS s
             JOIN workstreams AS w ON w.id = s.workstream_id
             WHERE s.session_id = ?1 ORDER BY s.start_offset",
        )
        .expect("prepare");
    stmt.query_map([session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(|s| s.expect("segment"))
        .collect()
}

fn named(start: i64, name: &str) -> (i64, String, bool) {
    (start, name.to_string(), false)
}

fn workstream_id(conn: &Connection, name: &str) -> i64 {
    conn.query_row("SELECT id FROM workstreams WHERE name = ?1", [name], |r| {
        r.get(0)
    })
    .expect("workstream")
}

/// The merge-resolved workstream name each event of `session`/`agent`
/// counts toward, in transcript order.
fn event_names(conn: &Connection, session: &str, agent: Option<&str>) -> Vec<Option<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT w.name FROM events AS e
             JOIN event_workstreams AS x ON x.event = e.id
             LEFT JOIN workstreams AS w ON w.id = x.workstream
             WHERE e.session_id = ?1 AND e.agent_id IS ?2 ORDER BY e.byte_offset",
        )
        .expect("prepare");
    stmt.query_map(params![session, agent], |r| r.get(0))
        .expect("query")
        .map(|n| n.expect("event"))
        .collect()
}

/// Everything grouping writes, keyed by names (ids differ between builds).
fn snapshot(conn: &Connection) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
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
        "SELECT 'ws ' || name || ' ' || fallback || ' ' || ifnull(merged_into, '-') FROM workstreams",
    );
    rows(
        "SELECT 'seg ' || s.session_id || ' ' || s.start_offset || ' ' || ifnull(s.started_at, '-')
             || ' ' || w.name FROM segments AS s JOIN workstreams AS w ON w.id = s.workstream_id",
    );
    rows(
        "SELECT 'session ' || s.session_id || ' ' || ifnull(w.name, '-') || ' '
             || ifnull(s.started_at, '-') || ' ' || ifnull(s.process_root, '-')
         FROM sessions AS s LEFT JOIN workstreams AS w ON w.id = s.workstream_id",
    );
    rows(
        "SELECT 'agent ' || a.session_id || ' ' || a.agent_id || ' ' || ifnull(s.start_offset, '-')
         FROM agents AS a LEFT JOIN segments AS s ON s.id = a.segment_id
         WHERE a.agent_id IS NOT NULL",
    );
    rows(
        "SELECT 'edge ' || p.session_id || ' -> ' || c.session_id FROM edges AS e
         JOIN agents AS p ON p.id = e.parent JOIN agents AS c ON c.id = e.child
         WHERE e.kind = 'continuation'",
    );
    out
}

fn built(name: &str) -> (Temp, Store, Connection) {
    let tmp = Temp::new(name);
    let mut store = Store::open(tmp.db()).expect("open");
    cold(&mut store);
    let conn = Store::open_reader(tmp.db()).expect("reader");
    (tmp, store, conn)
}

#[test]
fn a_late_rename_backfills_the_whole_file() {
    let (_tmp, _store, conn) = built("backfill");
    assert_eq!(segments(&conn, C1), vec![named(0, "demo-repo-81")]);
    assert_eq!(segments(&conn, D1), vec![named(0, "demo-repo-90")]);
    let names = event_names(&conn, C1, None);
    assert!(!names.is_empty());
    assert!(
        names.iter().all(|n| n.as_deref() == Some("demo-repo-81")),
        "records before the rename take its name: {names:?}"
    );
}

#[test]
fn a_mid_file_rename_splits_at_its_title_record_and_repeats_collapse() {
    let (_tmp, _store, conn) = built("split");
    // C9 repeats demo-repo-81 before the rename: no extra segment.
    assert_eq!(
        segments(&conn, C9),
        vec![
            named(0, "demo-repo-81"),
            named(title_offset(C9, "demo-repo-82"), "demo-repo-82"),
        ]
    );
    let names = event_names(&conn, C9, None);
    let split = names
        .iter()
        .position(|n| n.as_deref() == Some("demo-repo-82"))
        .expect("an event after the rename");
    assert!(
        names[..split]
            .iter()
            .all(|n| n.as_deref() == Some("demo-repo-81"))
    );
    assert!(
        names[split..]
            .iter()
            .all(|n| n.as_deref() == Some("demo-repo-82"))
    );
}

#[test]
fn a_carried_name_prelude_with_no_reply_merges_forward() {
    let (_tmp, _store, conn) = built("prelude");
    // C5 opens with demo-repo-82 carried over by /clear, then /rename: the
    // prelude joins demo-repo-83, and the repeated title adds nothing.
    assert_eq!(segments(&conn, C5), vec![named(0, "demo-repo-83")]);
    let in_82: i64 = conn
        .query_row(
            "SELECT count(*) FROM segments WHERE workstream_id = ?1",
            [workstream_id(&conn, "demo-repo-82")],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(in_82, 1, "demo-repo-82 holds only C9's second segment");
}

#[test]
fn the_final_segment_is_kept_without_a_reply() {
    let (_tmp, _store, conn) = built("final");
    assert_eq!(
        segments(&conn, D2),
        vec![
            named(0, "demo-repo-90"),
            named(title_offset(D2, "demo-repo-91"), "demo-repo-91"),
        ]
    );
    let current: String = conn
        .query_row(
            "SELECT w.name FROM sessions AS s JOIN workstreams AS w ON w.id = s.workstream_id
             WHERE s.session_id = ?1",
            [D2],
            |r| r.get(0),
        )
        .expect("session workstream");
    assert_eq!(
        current, "demo-repo-91",
        "a session's workstream is its latest name"
    );
}

#[test]
fn names_span_files_the_claude_config_81_shape() {
    let (_tmp, _store, conn) = built("span");
    let sessions_of = |name: &str| -> BTreeSet<String> {
        let mut stmt = conn
            .prepare(
                "SELECT s.session_id FROM segments AS s JOIN workstreams AS w
                 ON w.id = s.workstream_id WHERE w.name = ?1",
            )
            .expect("prepare");
        stmt.query_map([name], |r| r.get(0))
            .expect("query")
            .map(|s| s.expect("session"))
            .collect()
    };
    let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(sessions_of("demo-repo-81"), set(&[C1, C9]));
    assert_eq!(sessions_of("demo-repo-90"), set(&[D1, D2]));
    assert_eq!(sessions_of("demo-repo-83"), set(&[C5]));
}

#[test]
fn an_unnamed_session_falls_back_to_its_dominant_cwd_and_branch() {
    let (_tmp, _store, conn) = built("fallback");
    let fallback = (0, FALLBACK.to_string(), true);
    // E1's first record sits in /work/demo-repo on main; most sit in
    // packages/core on feat/9-lorem. E2 shares that key.
    assert_eq!(segments(&conn, E1), vec![fallback.clone()]);
    assert_eq!(segments(&conn, E2), vec![fallback]);
    let named_too: i64 = conn
        .query_row(
            "SELECT count(*) FROM workstreams WHERE fallback = 1",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        named_too, 1,
        "named sessions never get a fallback workstream"
    );
}

#[test]
fn subagents_join_the_segment_live_when_they_started() {
    let (_tmp, _store, conn) = built("subagents");
    let names = |agent: &str| event_names(&conn, C9, Some(agent));
    assert_eq!(
        names("a3000000000000001"),
        vec![Some("demo-repo-81".to_string()); 2]
    );
    assert_eq!(
        names("a3000000000000002"),
        vec![Some("demo-repo-82".to_string()); 2]
    );
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
fn incremental_ingest_converges_to_a_cold_build_and_regrouping_is_idempotent() {
    let (_cold_tmp, mut cold_store, cold_conn) = built("cold");
    let want = snapshot(&cold_conn);
    assert!(want.iter().any(|l| l.starts_with("seg ")), "{want:?}");

    let tmp = Temp::new("incremental");
    let mut store = Store::open(tmp.db()).expect("open");
    let conn = Store::open_reader(tmp.db()).expect("reader");
    // Subagent files first, then one line per batch, round-robin across files.
    let mut files: Vec<Vec<TailEvent>> = transcripts().iter().map(|p| records(p)).collect();
    files.sort_by_key(|f| {
        let TailEvent::Record { path, .. } = &f[0] else {
            unreachable!()
        };
        !path.to_string_lossy().contains("subagents")
    });
    let mut checked_before_rename = false;
    let mut round = 0;
    while files.iter().any(|f| round < f.len()) {
        for file in &files {
            if let Some(event) = file.get(round) {
                store.ingest(&batch(vec![event.clone()])).expect("ingest");
            }
        }
        round += 1;
        // C1 before its late rename: grouped by the cwd/branch fallback.
        if round == 3 {
            assert_eq!(
                segments(&conn, C1),
                vec![(0, "/work/demo-repo @ main".to_string(), true)]
            );
            checked_before_rename = true;
        }
    }
    assert!(checked_before_rename);
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    assert_eq!(snapshot(&conn), want, "incremental == cold");

    // Re-ingest and regroup: nothing moves, not even ids.
    let ids = |c: &Connection| -> Vec<(i64, i64)> {
        let mut stmt = c
            .prepare("SELECT id, workstream_id FROM segments ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    };
    let before = ids(&cold_conn);
    cold(&mut cold_store);
    cold_store.regroup_all().expect("regroup");
    assert_eq!(snapshot(&cold_conn), want);
    assert_eq!(ids(&cold_conn), before);
}

#[test]
fn a_manual_merge_survives_regrouping_and_re_ingest_and_can_be_undone() {
    let (_tmp, mut store, conn) = built("merge");
    let w81 = workstream_id(&conn, "demo-repo-81");
    let w82 = workstream_id(&conn, "demo-repo-82");
    let w83 = workstream_id(&conn, "demo-repo-83");
    store.merge_workstream(w82, w81).expect("merge 82 into 81");
    store.merge_workstream(w83, w82).expect("merge 83 into 82");
    assert!(
        store.merge_workstream(w81, w83).is_err(),
        "a merge that would form a cycle is refused"
    );
    assert!(store.merge_workstream(w81, w81).is_err());

    cold(&mut store);
    store.regroup_all().expect("regroup");
    let all_81 = |session: &str| {
        event_names(&conn, session, None)
            .iter()
            .all(|n| n.as_deref() == Some("demo-repo-81"))
    };
    assert!(all_81(C9), "82 reads as 81");
    assert!(all_81(C5), "83 -> 82 -> 81");
    assert_eq!(
        workstream_id(&conn, "demo-repo-82"),
        w82,
        "merged rows stay"
    );

    store.unmerge_workstream(w82).expect("unmerge");
    assert!(!all_81(C9));
    assert!(all_81(C1));
    let c5: Vec<_> = event_names(&conn, C5, None);
    assert!(
        c5.iter().all(|n| n.as_deref() == Some("demo-repo-82")),
        "83 still follows 82"
    );
    assert!(store.unmerge_workstream(-1).is_err(), "unknown workstream");
}
