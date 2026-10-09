//! Cost and time rollups (#14) over the synthetic fixture in
//! `tests/fixtures/cost-home`: parity with claude-config's `audit.mjs`
//! (its numbers committed in `tests/fixtures/audit-golden.json`; CI re-runs
//! audit.mjs against it), hand-computed time values, context %, workstream
//! $, and an incremental build converging to a cold one. Every db lives in
//! a temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use strate_core::cost::Usage;
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

const K1: &str = "5e55f0f0-0000-4000-8000-0000000000f1";
const K2: &str = "5e55f0f0-0000-4000-8000-0000000000f2";
const SUB: &str = "a4000000000000001";

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-cost-{name}-{}", std::process::id()));
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

fn root() -> PathBuf {
    fixtures().join("cost-home")
}

fn golden() -> Value {
    let text = fs::read_to_string(fixtures().join("audit-golden.json")).expect("golden");
    serde_json::from_str(&text).expect("golden json")
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
fn cold(t: &Temp) -> Connection {
    let mut store = Store::open(t.db()).expect("open");
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    for path in transcripts() {
        store.ingest(&batch(records(&path))).expect("ingest");
    }
    Store::open_reader(t.db()).expect("reader")
}

fn within_1pct(want: f64, got: f64) -> bool {
    (want - got).abs() <= 0.01 * want.abs().max(got.abs())
}

fn session_row<T: rusqlite::types::FromSql>(db: &Connection, column: &str, sid: &str) -> T {
    db.query_row(
        &format!("SELECT {column} FROM sessions WHERE session_id = ?1"),
        [sid],
        |r| r.get(0),
    )
    .expect(column)
}

fn agent_row<T: rusqlite::types::FromSql>(
    db: &Connection,
    column: &str,
    sid: &str,
    agent: Option<&str>,
) -> T {
    db.query_row(
        &format!("SELECT {column} FROM agents WHERE session_id = ?1 AND agent_id IS ?2"),
        params![sid, agent],
        |r| r.get(0),
    )
    .expect(column)
}

#[test]
fn session_and_agent_costs_match_audit_within_one_percent() {
    let t = Temp::new("parity-usd");
    let db = cold(&t);
    let g = golden();
    let mut total = 0.0;
    for s in g["sessions"].as_array().expect("sessions") {
        let sid = s["sessionId"].as_str().expect("id");
        let usd: f64 = session_row(&db, "cost_usd", sid);
        let want = s["usd"].as_f64().expect("usd");
        assert!(within_1pct(want, usd), "{sid}: audit {want}, strate {usd}");
        let requests: i64 = db
            .query_row(
                "SELECT count(*) FROM requests WHERE session_id = ?1",
                [sid],
                |r| r.get(0),
            )
            .expect("requests");
        assert_eq!(Some(requests), s["requests"].as_i64(), "{sid} requests");
        let sub: f64 = db
            .query_row(
                "SELECT total(cost_usd) FROM agents WHERE session_id = ?1 AND agent_id IS NOT NULL",
                [sid],
                |r| r.get(0),
            )
            .expect("subagent $");
        let want = s["subagentUsd"].as_f64().expect("subagentUsd");
        assert!(
            within_1pct(want, sub),
            "{sid} subagents: audit {want}, strate {sub}"
        );
        total += usd;
    }
    let want = g["totals"]["usd"].as_f64().expect("total");
    assert!(
        within_1pct(want, total),
        "total: audit {want}, strate {total}"
    );
    let requests: i64 = db
        .query_row("SELECT count(*) FROM requests", [], |r| r.get(0))
        .expect("requests");
    assert_eq!(Some(requests), g["totals"]["requests"].as_i64());

    for run in g["time"]["agentRuns"].as_array().expect("runs") {
        let (sid, id) = (run["sessionId"].as_str(), run["id"].as_str());
        let sid = sid.expect("session");
        let usd: f64 = agent_row(&db, "cost_usd", sid, id);
        let want = run["usd"].as_f64().expect("usd");
        assert!(within_1pct(want, usd), "{id:?}: audit {want}, strate {usd}");
        let run_ms: i64 = agent_row(&db, "run_ms", sid, id);
        assert_eq!(Some(run_ms), run["wallMs"].as_i64(), "{id:?} wall");
    }
}

#[test]
fn token_totals_match_audit_exactly() {
    let t = Temp::new("parity-tokens");
    let db = cold(&t);
    let g = golden();
    let rows: Vec<(String, Option<f64>, String)> = db
        .prepare("SELECT model, cost_usd, usage FROM requests")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let mut tokens = [0u64; 5];
    let mut unpriced = serde_json::Map::new();
    for (model, usd, usage) in rows {
        let u = Usage::from_json(&serde_json::from_str(&usage).expect("usage json"));
        let parts = [
            u.input,
            u.output,
            u.cache_read,
            u.cache_write_5m,
            u.cache_write_1h,
        ];
        for (sum, n) in tokens.iter_mut().zip(parts) {
            *sum += n;
        }
        if usd.is_none() {
            let x = unpriced
                .entry(model)
                .or_insert_with(|| json!({"requests": 0, "tokens": 0}));
            x["requests"] = json!(x["requests"].as_u64().expect("n") + 1);
            x["tokens"] = json!(x["tokens"].as_u64().expect("n") + parts.iter().sum::<u64>());
        }
    }
    let want = &g["totals"]["tokens"];
    let want: Vec<u64> = [
        "input",
        "output",
        "cacheRead",
        "cacheWrite5m",
        "cacheWrite1h",
    ]
    .iter()
    .map(|k| want[k].as_u64().expect(k))
    .collect();
    assert_eq!(tokens.to_vec(), want);
    assert_eq!(Value::Object(unpriced), g["unpriced"]);
}

#[test]
fn session_times_match_audit() {
    let t = Temp::new("parity-time");
    let db = cold(&t);
    for s in golden()["time"]["sessions"].as_array().expect("sessions") {
        let sid = s["sessionId"].as_str().expect("id");
        for (column, key) in [
            ("run_ms", "wallMs"),
            ("active_ms", "activeMs"),
            ("wait_ms", "userMs"),
        ] {
            let got: i64 = session_row(&db, column, sid);
            assert_eq!(Some(got), s[key].as_i64(), "{sid} {column}");
        }
    }
}

#[test]
fn time_rollups_match_hand_computed_values() {
    let t = Temp::new("hand-time");
    let db = cold(&t);
    // K1, seconds from 10:00:00. Main records at 0, 5, 8, 128, 130, 315,
    // 320, 440, 445, 1800, 1804, 1806; subagent records at 132, 192, 252,
    // 312. Wall: 1806 - 0 = 1806 s. The only gap over 10 min is 445 ->
    // 1800 = 1355 s, so active = 1806 - 1355 = 451 s. Waiting: the
    // AskUserQuestion 8 -> 128 (120 s) plus the wait for the 440 s prompt
    // since the last work at 320 (120 s) = 240 s; the wait for the 1800 s
    // prompt lies inside the idle gap. Agent-hours: the one subagent's
    // 312 - 132 = 180 s.
    let session = |column| session_row::<i64>(&db, column, K1);
    assert_eq!(session("run_ms"), 1_806_000);
    assert_eq!(session("active_ms"), 451_000);
    assert_eq!(session("wait_ms"), 240_000);
    assert_eq!(session("agent_ms"), 180_000);
    // The orchestrator's own records span the whole session too.
    assert_eq!(agent_row::<i64>(&db, "run_ms", K1, None), 1_806_000);
    assert_eq!(agent_row::<i64>(&db, "wait_ms", K1, None), 240_000);
    // The subagent: 132 -> 312 = 180 s; its one prompt has no work before
    // it, so it waits 0.
    assert_eq!(agent_row::<i64>(&db, "run_ms", K1, Some(SUB)), 180_000);
    assert_eq!(agent_row::<i64>(&db, "wait_ms", K1, Some(SUB)), 0);
    // K2, seconds from 09:00:00: records at 0, 3, 60, 62, no gap. Wall and
    // active 62 s; waiting for the 60 s prompt since the reply at 3 = 57 s;
    // no subagents.
    let session = |column| session_row::<i64>(&db, column, K2);
    assert_eq!(session("run_ms"), 62_000);
    assert_eq!(session("active_ms"), 62_000);
    assert_eq!(session("wait_ms"), 57_000);
    assert_eq!(session("agent_ms"), 0);
}

#[test]
fn context_pct_is_the_latest_request_depth_over_the_model_window() {
    let t = Temp::new("context");
    let db = cold(&t);
    let context = |sid, agent| {
        (
            agent_row::<String>(&db, "model", sid, agent),
            agent_row::<i64>(&db, "context_tokens", sid, agent),
            agent_row::<f64>(&db, "context_pct", sid, agent),
        )
    };
    // K1's last real request (req_c4): 6 input + 150000 read + 50000 write
    // = 200006 of claude-opus-5-5's 1M = 20.0006%. The <synthetic> record
    // after it changes neither the depth nor the model.
    let (model, depth, pct) = context(K1, None);
    assert_eq!((model.as_str(), depth), ("claude-opus-5-5", 200_006));
    assert!((pct - 20.0006).abs() < 1e-9, "{pct}");
    // The subagent's req_s2: 10 + 8000 + 0 = 8010 of 1M = 0.801%.
    let (_, depth, pct) = context(K1, Some(SUB));
    assert_eq!(depth, 8010);
    assert!((pct - 0.801).abs() < 1e-9, "{pct}");
    // K2's req_d2 on a dated haiku snapshot: 8 + 48000 + 2000 = 50008 of
    // 200k = 25.004%.
    let (model, depth, pct) = context(K2, None);
    assert_eq!(
        (model.as_str(), depth),
        ("claude-haiku-4-5-20251001", 50_008)
    );
    assert!((pct - 25.004).abs() < 1e-9, "{pct}");
}

#[test]
fn unpriced_requests_are_counted_never_priced_and_synthetic_is_neither() {
    let t = Temp::new("unpriced");
    let db = cold(&t);
    assert_eq!(session_row::<i64>(&db, "unpriced", K2), 1);
    assert_eq!(session_row::<i64>(&db, "unpriced", K1), 0);
    let unpriced: Option<f64> = db
        .query_row(
            "SELECT cost_usd FROM requests WHERE model = 'claude-lorem-1'",
            [],
            |r| r.get(0),
        )
        .expect("unpriced request");
    assert_eq!(unpriced, None, "unpriced is NULL, never $0");
    let synthetic: i64 = db
        .query_row(
            "SELECT count(*) FROM events WHERE usage IS NOT NULL AND raw LIKE '%<synthetic>%'",
            [],
            |r| r.get(0),
        )
        .expect("synthetic");
    assert_eq!(synthetic, 0);
    // Streaming partials: one request, the record with the most output.
    let partials: Vec<i64> = db
        .prepare("SELECT output_tokens FROM requests WHERE request_key = 'req_c1'")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(partials, vec![40]);
}

#[test]
fn workstream_cost_is_queryable_through_event_workstreams() {
    let t = Temp::new("workstream");
    let db = cold(&t);
    let rows: Vec<(String, f64, i64)> = db
        .prepare(
            "SELECT w.name, c.cost_usd, c.unpriced FROM workstream_costs AS c
             JOIN workstreams AS w ON w.id = c.workstream ORDER BY w.name",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let names: Vec<(&str, i64)> = rows.iter().map(|(n, _, u)| (n.as_str(), *u)).collect();
    assert_eq!(
        names,
        vec![("/work/cost-other @ main", 1), ("cost-repo-14", 0)]
    );
    // The named workstream holds K1 and its subagent.
    let k1: f64 = session_row(&db, "cost_usd", K1);
    assert!((rows[1].1 - k1).abs() < 1e-12, "{} vs {k1}", rows[1].1);
    let k2: f64 = session_row(&db, "cost_usd", K2);
    assert!((rows[0].1 - k2).abs() < 1e-12, "{} vs {k2}", rows[0].1);
}

/// Every rollup, with $ to 9 places: sums over the same requests may add
/// in a different order.
fn rollups(db: &Connection) -> Vec<String> {
    let mut out: Vec<String> = db
        .prepare(
            "SELECT session_id, printf('%.9f', cost_usd), unpriced, run_ms, active_ms, wait_ms,
                 agent_ms FROM sessions ORDER BY session_id",
        )
        .expect("sessions")
        .query_map([], |r| {
            Ok(format!(
                "{:?}",
                (0..7)
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect::<Result<Vec<_>, _>>()?
            ))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    out.extend(
        db.prepare(
            "SELECT session_id, agent_id, model, printf('%.9f', cost_usd), unpriced, run_ms,
                 wait_ms, context_tokens, printf('%.9f', context_pct)
             FROM agents ORDER BY session_id, agent_id",
        )
        .expect("agents")
        .query_map([], |r| {
            Ok(format!(
                "{:?}",
                (0..9)
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect::<Result<Vec<_>, _>>()?
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows"),
    );
    out
}

#[test]
fn an_incremental_and_repeated_ingest_equals_a_cold_build() {
    let a = Temp::new("cold");
    let want = rollups(&cold(&a));

    let b = Temp::new("incremental");
    let mut store = Store::open(b.db()).expect("open");
    // Files in reverse order (the subagent before its session), one line
    // per batch, the graph last; then everything again.
    for _ in 0..2 {
        for path in transcripts().iter().rev() {
            for event in records(path) {
                store.ingest(&batch(vec![event])).expect("ingest");
            }
        }
        store
            .ingest_graph(&discover(&root()).expect("discover"))
            .expect("graph");
    }
    drop(store);
    assert_eq!(rollups(&Store::open_reader(b.db()).expect("reader")), want);
}

#[test]
fn a_request_replayed_into_another_session_counts_once_toward_the_larger() {
    let t = Temp::new("cross-session");
    let mut store = Store::open(t.db()).expect("open");
    let line = |sid: &str, uuid: &str, output: u64| {
        json!({
            "type": "assistant", "uuid": uuid, "sessionId": sid,
            "timestamp": "2026-10-03T10:00:00.000Z", "requestId": "req_x",
            "message": {"id": "msg_x", "model": "claude-haiku-4-5", "content": [],
                "usage": {"input_tokens": 0, "output_tokens": output}}
        })
    };
    let ingest = |store: &mut Store, sid: &str, value: Value| {
        let path: Arc<Path> = t.0.join(format!("projects/-work-x/{sid}.jsonl")).into();
        store
            .ingest(&batch(vec![TailEvent::Record {
                path,
                offset: 0,
                value,
            }]))
            .expect("ingest");
    };
    // haiku output is $5/MTok: 100 tokens = $0.0005, 300 = $0.0015.
    ingest(&mut store, "s-old", line("s-old", "x-1", 100));
    let db = Store::open_reader(t.db()).expect("reader");
    let cost = |sid| session_row::<Option<f64>>(&db, "cost_usd", sid);
    assert!((cost("s-old").expect("priced") - 0.0005).abs() < 1e-12);
    // The replay in another file carries more output: it owns the request
    // now, and the first session's rollup drops it.
    ingest(&mut store, "s-new", line("s-new", "x-2", 300));
    assert_eq!(cost("s-old"), None);
    assert!((cost("s-new").expect("priced") - 0.0015).abs() < 1e-12);
}
