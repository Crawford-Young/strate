//! Audit cells (#26): one row per (session, agent, window, local day).
//! Invariants: the cells sum to the stored session and agent rollups and to
//! the session's request tokens. Hand-computed cases: a window across local
//! midnight in America/Toronto, DST days (25 h and 23 h), a renamed
//! (2-segment) transcript, a request with no timestamp, a zone change
//! forcing a recompute, and the row timeline on the DST days. Every
//! transcript is synthetic and every db lives in a temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use strate_core::cost::Usage;
use strate_core::discovery::discover;
use strate_core::store::Store;
use strate_core::store::views::{AuditFilter, SpanKind, TimelineDay, TimelineSpan, audit_timeline};
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

const TORONTO: &str = "America/Toronto";
const HAIKU: &str = "claude-haiku-4-5";
const MIN: i64 = 60_000;

mod common;

use common::{Temp, fixture_home, transcripts};

/// Where session `sid`'s transcript (or its subagent's) would live in `t`.
fn transcript(t: &Temp, sid: &str, agent: Option<&str>) -> PathBuf {
    let project = t.path().join("projects").join("-work-audit");
    match agent {
        None => project.join(format!("{sid}.jsonl")),
        Some(a) => project
            .join(sid)
            .join("subagents")
            .join(format!("agent-{a}.jsonl")),
    }
}

/// One batch holding `lines` as records 100 bytes apart. Unlike
/// `common::batch` it takes records as values and checkpoints 100 bytes
/// past the last.
fn batch(path: &Path, lines: Vec<Value>) -> Batch {
    let path: Arc<Path> = path.into();
    let n = lines.len() as u64;
    let events = lines
        .into_iter()
        .enumerate()
        .map(|(i, value)| TailEvent::Record {
            path: path.clone(),
            offset: i as u64 * 100,
            value,
        })
        .collect();
    Batch {
        path,
        events,
        checkpoint: Some(Checkpoint {
            offset: n * 100,
            identity: FileIdentity {
                volume: 0,
                index: 0,
            },
        }),
    }
}

fn title(name: &str) -> Value {
    json!({"type": "custom-title", "customTitle": name})
}

fn prompt(uuid: &str, at: &str) -> Value {
    json!({"type": "user", "uuid": uuid, "timestamp": at, "message": {"content": "Lorem."}})
}

/// A request of `model` with `usage`; `at` `None` leaves out the timestamp.
fn reply(uuid: &str, at: Option<&str>, request: &str, model: &str, usage: Value) -> Value {
    let mut v = json!({"type": "assistant", "uuid": uuid, "requestId": request,
        "message": {"id": format!("msg_{request}"), "model": model, "usage": usage,
            "content": [{"type": "text", "text": "Lorem."}]}});
    if let Some(at) = at {
        v["timestamp"] = json!(at);
    }
    v
}

fn tokens(input: u64, output: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output})
}

#[derive(Debug, Clone, PartialEq)]
struct Cell {
    agent: Option<String>,
    window: Option<String>,
    day: Option<String>,
    wall_ms: i64,
    active_ms: i64,
    wait_ms: i64,
    cost_usd: Option<f64>,
    unpriced: i64,
    tokens: [i64; 4],
    model: Option<String>,
}

/// The session's cells, each with its window's workstream name, ordered by
/// agent, day, window start and model.
fn cells(db: &Connection, sid: &str) -> Vec<Cell> {
    db.prepare(
        "SELECT c.agent_id, w.name, c.day, c.wall_ms, c.active_ms, c.wait_ms, c.cost_usd,
             c.unpriced, c.input_tokens, c.output_tokens, c.cache_read_tokens,
             c.cache_write_tokens, c.model
         FROM audit_cells AS c
         LEFT JOIN segments AS s ON s.id = c.segment_id
         LEFT JOIN workstreams AS w ON w.id = s.workstream_id
         WHERE c.session_id = ?1
         ORDER BY c.agent_id IS NOT NULL, c.agent_id, c.day, s.start_offset, c.model",
    )
    .expect("cells")
    .query_map([sid], |r| {
        Ok(Cell {
            agent: r.get(0)?,
            window: r.get(1)?,
            day: r.get(2)?,
            wall_ms: r.get(3)?,
            active_ms: r.get(4)?,
            wait_ms: r.get(5)?,
            cost_usd: r.get(6)?,
            unpriced: r.get(7)?,
            tokens: [r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?],
            model: r.get(12)?,
        })
    })
    .expect("query")
    .collect::<rusqlite::Result<_>>()
    .expect("rows")
}

fn close(want: f64, got: f64) -> bool {
    (want - got).abs() <= 1e-12 * want.abs().max(1.0)
}

/// A stored session or agent rollup: its id, $, unpriced count, and
/// run/active/wait ms.
struct Rollup {
    id: Option<String>,
    cost_usd: Option<f64>,
    unpriced: Option<i64>,
    times: [Option<i64>; 3],
}

/// The rollups `sql` selects, in that column order.
fn rollups(db: &Connection, sql: &str, args: impl rusqlite::Params) -> Vec<Rollup> {
    db.prepare(sql)
        .expect("rollups")
        .query_map(args, |r| {
            Ok(Rollup {
                id: r.get(0)?,
                cost_usd: r.get(1)?,
                unpriced: r.get(2)?,
                times: [r.get(3)?, r.get(4)?, r.get(5)?],
            })
        })
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

/// Asserts every invariant of every stored session's cells.
fn invariants(db: &Connection) {
    let sessions = rollups(
        db,
        "SELECT session_id, cost_usd, unpriced, run_ms, active_ms, wait_ms FROM sessions",
        [],
    );
    assert!(!sessions.is_empty(), "nothing stored");
    let asks_past_wait: i64 = db
        .query_row(
            "SELECT count(*) FROM audit_cells WHERE ask_ms < 0 OR ask_ms > wait_ms",
            [],
            |r| r.get(0),
        )
        .expect("ask_ms");
    assert_eq!(
        asks_past_wait, 0,
        "a cell's questions are part of its waiting"
    );
    for session in sessions {
        let sid = session.id.expect("session id");
        let (cost, unpriced) = (session.cost_usd, session.unpriced);
        let all = cells(db, &sid);
        let orchestrator: Vec<&Cell> = all.iter().filter(|c| c.agent.is_none()).collect();
        let time = |cs: &[&Cell]| -> [i64; 3] {
            cs.iter().fold([0; 3], |t, c| {
                [t[0] + c.wall_ms, t[1] + c.active_ms, t[2] + c.wait_ms]
            })
        };
        assert_eq!(
            time(&orchestrator),
            session.times.map(|t| t.unwrap_or(0)),
            "{sid}: orchestrator cells vs session run/active/wait"
        );
        let usd = |cs: &[&Cell]| -> f64 { cs.iter().filter_map(|c| c.cost_usd).sum() };
        let every: Vec<&Cell> = all.iter().collect();
        assert!(
            close(cost.unwrap_or(0.0), usd(&every)),
            "{sid}: cells ${} vs session ${cost:?}",
            usd(&every)
        );
        let count = |cs: &[&Cell]| -> i64 { cs.iter().map(|c| c.unpriced).sum() };
        assert_eq!(Some(count(&every)), unpriced, "{sid}: unpriced");

        let mut want = [0i64; 4];
        let usages: Vec<String> = db
            .prepare("SELECT usage FROM requests WHERE session_id = ?1")
            .expect("requests")
            .query_map([&sid], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        for usage in usages {
            let u = Usage::from_json(&serde_json::from_str(&usage).expect("usage json"));
            for (w, n) in want
                .iter_mut()
                .zip([u.input, u.output, u.cache_read, u.cache_creation])
            {
                *w += n as i64;
            }
        }
        let got = all.iter().fold([0i64; 4], |mut t, c| {
            for (t, n) in t.iter_mut().zip(c.tokens) {
                *t += n;
            }
            t
        });
        assert_eq!(got, want, "{sid}: tokens vs requests");

        // agents has no active_ms.
        let agents = rollups(
            db,
            "SELECT agent_id, cost_usd, unpriced, run_ms, NULL, wait_ms FROM agents
             WHERE session_id = ?1",
            [&sid],
        );
        for Rollup {
            id: agent,
            cost_usd: cost,
            unpriced,
            times: [run_ms, _, wait_ms],
        } in agents
        {
            let own: Vec<&Cell> = all.iter().filter(|c| c.agent == agent).collect();
            assert!(
                close(cost.unwrap_or(0.0), usd(&own)),
                "{sid}/{agent:?}: cells ${} vs agent ${cost:?}",
                usd(&own)
            );
            assert_eq!(Some(count(&own)), unpriced, "{sid}/{agent:?}: unpriced");
            // The orchestrator's run_ms is its own records' span, not the
            // session timeline its cells carry.
            if agent.is_some() {
                let [wall, _, wait] = time(&own);
                assert_eq!(
                    (wall, wait),
                    (run_ms.unwrap_or(0), wait_ms.unwrap_or(0)),
                    "{sid}/{agent:?}: run/wait"
                );
            }
        }
    }
}

fn reader(t: &Temp) -> Connection {
    Store::open_reader(t.db()).expect("reader")
}

/// Every transcript under the cost-home fixture, one batch each.
fn fixture_batches() -> Vec<Batch> {
    transcripts(&fixture_home("cost-home"))
        .iter()
        .map(|p| {
            let lines = fs::read_to_string(p).expect("read");
            batch(
                p,
                lines
                    .lines()
                    .map(|l| serde_json::from_str(l).expect("json"))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn the_cost_fixture_cells_sum_to_its_rollups_in_any_zone() {
    for zone in [TORONTO, "UTC", "Pacific/Kiritimati", "Pacific/Pago_Pago"] {
        let t = Temp::new(&format!("fixture-{}", zone.replace('/', "-")));
        let mut store = Store::open_in(t.db(), zone).expect("open");
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cost-home");
        store
            .ingest_graph(&discover(&home).expect("discover"))
            .expect("graph");
        for b in fixture_batches() {
            store.ingest(&b).expect("ingest");
        }
        let db = reader(&t);
        let n: i64 = db
            .query_row("SELECT count(*) FROM audit_cells", [], |r| r.get(0))
            .expect("count");
        assert!(n > 0, "{zone}: no cells");
        invariants(&db);
    }
}

/// 23:50 EDT on Oct 2 to 00:10 EDT on Oct 3: one request either side of
/// local midnight (04:00Z), each fed by a typed prompt.
fn across_midnight() -> Vec<Value> {
    vec![
        prompt("p1", "2026-10-03T03:50:00.000Z"),
        reply(
            "r1",
            Some("2026-10-03T03:55:00.000Z"),
            "req_1",
            HAIKU,
            tokens(1000, 200),
        ),
        prompt("p2", "2026-10-03T04:05:00.000Z"),
        reply(
            "r2",
            Some("2026-10-03T04:10:00.000Z"),
            "req_2",
            HAIKU,
            tokens(2000, 400),
        ),
    ]
}

#[test]
fn a_window_across_local_midnight_splits_into_two_days() {
    let t = Temp::new("midnight");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    store
        .ingest(&batch(&transcript(&t, "s1", None), across_midnight()))
        .expect("ingest");
    let db = reader(&t);
    invariants(&db);
    // Model 03:50-03:55 and 04:05-04:10; the wait for p2 runs 03:55-04:05,
    // 5 min each side of midnight. No gap exceeds 10 min, so no idle.
    // haiku: $1/M in, $5/M out: (1000 + 1000) / 1e6 and (2000 + 2000) / 1e6.
    let day = |d: &str, cost: f64, input: i64, output: i64| Cell {
        agent: None,
        window: None,
        day: Some(d.into()),
        wall_ms: 10 * MIN,
        active_ms: 10 * MIN,
        wait_ms: 5 * MIN,
        cost_usd: Some(cost),
        unpriced: 0,
        tokens: [input, output, 0, 0],
        model: Some(HAIKU.into()),
    };
    let got: Vec<Cell> = cells(&db, "s1")
        .into_iter()
        .map(|c| Cell { window: None, ..c })
        .collect();
    assert_eq!(got.len(), 2, "{got:#?}");
    for (got, want) in got.iter().zip([
        day("2026-10-02", 0.002, 1000, 200),
        day("2026-10-03", 0.004, 2000, 400),
    ]) {
        assert!(
            close(want.cost_usd.unwrap(), got.cost_usd.expect("priced")),
            "{got:?}"
        );
        assert_eq!(
            Cell {
                cost_usd: want.cost_usd,
                ..got.clone()
            },
            want
        );
    }
}

#[test]
fn dst_days_are_25_and_23_hours() {
    let t = Temp::new("dst");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    // Fall back: midnight Nov 1 is 04:00Z (EDT), Nov 2 is 05:00Z (EST).
    let fall = vec![
        prompt("f1", "2026-11-01T03:30:00.000Z"),
        reply(
            "f2",
            Some("2026-11-01T03:35:00.000Z"),
            "req_f1",
            HAIKU,
            tokens(1, 1),
        ),
        prompt("f3", "2026-11-02T05:20:00.000Z"),
        reply(
            "f4",
            Some("2026-11-02T05:30:00.000Z"),
            "req_f2",
            HAIKU,
            tokens(1, 1),
        ),
    ];
    // Spring forward: midnight Mar 8 is 05:00Z (EST), Mar 9 is 04:00Z (EDT).
    let spring = vec![
        prompt("s1", "2026-03-08T04:30:00.000Z"),
        reply(
            "s2",
            Some("2026-03-08T04:35:00.000Z"),
            "req_s1",
            HAIKU,
            tokens(1, 1),
        ),
        prompt("s3", "2026-03-09T04:20:00.000Z"),
        reply(
            "s4",
            Some("2026-03-09T04:30:00.000Z"),
            "req_s2",
            HAIKU,
            tokens(1, 1),
        ),
    ];
    store
        .ingest(&batch(&transcript(&t, "fall", None), fall))
        .expect("fall");
    store
        .ingest(&batch(&transcript(&t, "spring", None), spring))
        .expect("spring");
    let db = reader(&t);
    invariants(&db);
    // The long gap is idle (idle beats the wait for the prompt): only the
    // 5 min and 10 min requests are active.
    let shape = |sid: &str| -> Vec<(Option<String>, i64, i64, i64)> {
        cells(&db, sid)
            .into_iter()
            .map(|c| (c.day, c.wall_ms, c.active_ms, c.wait_ms))
            .collect()
    };
    let h = 60 * MIN;
    assert_eq!(
        shape("fall"),
        vec![
            (Some("2026-10-31".into()), 30 * MIN, 5 * MIN, 0),
            (Some("2026-11-01".into()), 25 * h, 0, 0),
            (Some("2026-11-02".into()), 30 * MIN, 10 * MIN, 0),
        ]
    );
    assert_eq!(
        shape("spring"),
        vec![
            (Some("2026-03-07".into()), 30 * MIN, 5 * MIN, 0),
            (Some("2026-03-08".into()), 23 * h, 0, 0),
            (Some("2026-03-09".into()), 30 * MIN, 10 * MIN, 0),
        ]
    );
}

#[test]
fn a_dst_days_timeline_sits_on_the_wall_clock() {
    let t = Temp::new("dst-timeline");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    // Spring forward on Mar 8 at 07:00Z (02:00 EST becomes 03:00 EDT): a
    // request 01:55 EST to 03:05 EDT, then one at 10:00 EDT, 9 h after
    // local midnight (05:00Z) on this 23 h day.
    let spring = vec![
        prompt("s1", "2026-03-08T06:55:00.000Z"),
        reply(
            "s2",
            Some("2026-03-08T07:05:00.000Z"),
            "req_s1",
            HAIKU,
            tokens(1, 1),
        ),
        prompt("s3", "2026-03-08T14:00:00.000Z"),
        reply(
            "s4",
            Some("2026-03-08T14:05:00.000Z"),
            "req_s2",
            HAIKU,
            tokens(1, 1),
        ),
    ];
    // Fall back on Nov 1 at 06:00Z (02:00 EDT becomes 01:00 EST): a request
    // 01:55 EDT to 01:05 EST, then one at 23:30 EST, 24.5 h after local
    // midnight (04:00Z) on this 25 h day.
    let fall = vec![
        prompt("f1", "2026-11-01T05:55:00.000Z"),
        reply(
            "f2",
            Some("2026-11-01T06:05:00.000Z"),
            "req_f1",
            HAIKU,
            tokens(1, 1),
        ),
        prompt("f3", "2026-11-02T04:30:00.000Z"),
        reply(
            "f4",
            Some("2026-11-02T04:40:00.000Z"),
            "req_f2",
            HAIKU,
            tokens(1, 1),
        ),
    ];
    store
        .ingest(&batch(&transcript(&t, "spring", None), spring))
        .expect("spring");
    store
        .ingest(&batch(&transcript(&t, "fall", None), fall))
        .expect("fall");
    let db = reader(&t);
    let year = AuditFilter {
        from: "2026-01-01".into(),
        to: "2026-12-31".into(),
        repos: vec![],
    };
    let days = audit_timeline(&db, &year, &[]).expect("timeline");
    // The DayTimeline axis is the 24 h wall clock, so a span sits at its
    // clock time. A request across the change is cut there: on Mar 8 the
    // skipped hour 02:00-03:00 holds nothing, and on Nov 1 the hour
    // 01:00-02:00 holds both passes. The gaps between requests are idle.
    let h = 60 * MIN;
    let active = |start_ms: i64, end_ms: i64| TimelineSpan {
        start_ms,
        end_ms,
        kind: SpanKind::Active,
    };
    assert_eq!(
        days,
        [
            TimelineDay {
                day: "2026-03-08".into(),
                spans: vec![
                    active(h + 55 * MIN, 2 * h),
                    active(3 * h, 3 * h + 5 * MIN),
                    active(10 * h, 10 * h + 5 * MIN),
                ],
                marks: vec![],
            },
            TimelineDay {
                day: "2026-11-01".into(),
                spans: vec![
                    active(h, h + 5 * MIN),
                    active(h + 55 * MIN, 2 * h),
                    active(23 * h + 30 * MIN, 23 * h + 40 * MIN),
                ],
                marks: vec![],
            },
        ]
    );
}

#[test]
fn a_renamed_transcript_splits_per_window_and_a_subagent_keeps_its_own() {
    let t = Temp::new("renamed");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    let cached = json!({"input_tokens": 10, "output_tokens": 20,
        "cache_read_input_tokens": 300, "cache_creation_input_tokens": 40});
    let main = vec![
        title("demo-9"),
        prompt("m1", "2026-10-03T10:00:00.000Z"),
        reply(
            "m2",
            Some("2026-10-03T10:05:00.000Z"),
            "req_m1",
            HAIKU,
            cached.clone(),
        ),
        title("demo-9-s2"),
        prompt("m3", "2026-10-03T10:10:00.000Z"),
        reply(
            "m4",
            Some("2026-10-03T10:15:00.000Z"),
            "req_m2",
            "claude-lorem-1",
            tokens(5, 6),
        ),
    ];
    let sub = vec![
        json!({"type": "user", "uuid": "a1", "timestamp": "2026-10-03T10:11:00.000Z",
            "isSidechain": true, "agentId": "a9", "message": {"content": "Lorem."}}),
        reply(
            "a2",
            Some("2026-10-03T10:13:00.000Z"),
            "req_a1",
            HAIKU,
            tokens(7, 8),
        ),
    ];
    store
        .ingest(&batch(&transcript(&t, "s1", None), main))
        .expect("main");
    store
        .ingest(&batch(&transcript(&t, "s1", Some("a9")), sub))
        .expect("sub");
    let db = reader(&t);
    invariants(&db);
    let got = cells(&db, "s1");
    type Shape<'a> = (Option<&'a str>, Option<&'a str>, i64, i64, Option<&'a str>);
    let shape: Vec<Shape> = got
        .iter()
        .map(|c| {
            (
                c.agent.as_deref(),
                c.window.as_deref(),
                c.wall_ms,
                c.wait_ms,
                c.model.as_deref(),
            )
        })
        .collect();
    // demo-9 runs 10:00-10:10 (its 10:05-10:10 wait for m3 included);
    // demo-9-s2 from its first record at 10:10 to 10:15. The subagent
    // (10:11-10:13) started under demo-9-s2.
    assert_eq!(
        shape,
        vec![
            (None, Some("demo-9"), 10 * MIN, 5 * MIN, Some(HAIKU)),
            (None, Some("demo-9-s2"), 5 * MIN, 0, Some("claude-lorem-1")),
            (Some("a9"), Some("demo-9-s2"), 2 * MIN, 0, Some(HAIKU)),
        ]
    );
    assert_eq!(got[0].tokens, [10, 20, 300, 40]);
    assert_eq!((got[1].cost_usd, got[1].unpriced), (None, 1), "unpriced");
    assert_eq!(got[2].tokens, [7, 8, 0, 0]);
    assert!(got.iter().all(|c| c.day.as_deref() == Some("2026-10-03")));
}

#[test]
fn a_request_with_no_timestamp_lands_in_a_cell_with_no_day() {
    let t = Temp::new("no-day");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    let mut lines = across_midnight();
    lines.push(reply("r3", None, "req_3", HAIKU, tokens(3000, 0)));
    store
        .ingest(&batch(&transcript(&t, "s1", None), lines))
        .expect("ingest");
    let db = reader(&t);
    invariants(&db);
    let dayless: Vec<Cell> = cells(&db, "s1")
        .into_iter()
        .filter(|c| c.day.is_none())
        .collect();
    assert_eq!(dayless.len(), 1, "{dayless:#?}");
    let c = &dayless[0];
    assert_eq!((c.wall_ms, c.tokens), (0, [3000, 0, 0, 0]));
    assert!(close(0.003, c.cost_usd.expect("priced")));
}

#[test]
fn a_zone_change_recomputes_every_cell_and_the_same_zone_does_not() {
    let t = Temp::new("tz-change");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest(&batch(&transcript(&t, "s1", None), across_midnight()))
        .expect("ingest");
    drop(store);
    let days = || -> Vec<Option<String>> {
        cells(&reader(&t), "s1")
            .into_iter()
            .map(|c| c.day)
            .collect()
    };
    let tz = || -> String {
        reader(&t)
            .query_row("SELECT value FROM meta WHERE key = 'tz'", [], |r| r.get(0))
            .expect("tz meta")
    };
    assert_eq!(days(), vec![Some("2026-10-03".into())]);
    assert_eq!(tz(), "UTC");

    drop(Store::open_in(t.db(), TORONTO).expect("reopen"));
    assert_eq!(
        days(),
        vec![Some("2026-10-02".into()), Some("2026-10-03".into())]
    );
    assert_eq!(tz(), TORONTO);
    invariants(&reader(&t));

    // At the stored zone, open leaves the cells alone.
    Connection::open(t.db())
        .expect("db")
        .execute("UPDATE audit_cells SET wall_ms = -1", params![])
        .expect("marker");
    drop(Store::open_in(t.db(), TORONTO).expect("reopen"));
    let walls: Vec<i64> = cells(&reader(&t), "s1")
        .into_iter()
        .map(|c| c.wall_ms)
        .collect();
    assert_eq!(walls, vec![-1, -1]);
}

#[test]
fn a_model_switch_splits_a_cell_per_model_with_the_time_on_one() {
    let t = Temp::new("model-switch");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    const OPUS: &str = "claude-opus-5-5";
    let lines = vec![
        prompt("p1", "2026-10-03T10:00:00.000Z"),
        reply(
            "r1",
            Some("2026-10-03T10:02:00.000Z"),
            "req_1",
            HAIKU,
            tokens(1000, 200),
        ),
        prompt("p2", "2026-10-03T10:04:00.000Z"),
        reply(
            "r2",
            Some("2026-10-03T10:06:00.000Z"),
            "req_2",
            OPUS,
            tokens(1000, 100),
        ),
        prompt("p3", "2026-10-03T10:08:00.000Z"),
        reply(
            "r3",
            Some("2026-10-03T10:10:00.000Z"),
            "req_3",
            OPUS,
            tokens(2000, 100),
        ),
    ];
    store
        .ingest(&batch(&transcript(&t, "s1", None), lines))
        .expect("ingest");
    let db = reader(&t);
    invariants(&db);
    let got = cells(&db, "s1");
    assert_eq!(got.len(), 2, "{got:#?}");
    let (haiku, opus) = (&got[0], &got[1]);
    // haiku $1/$5 per M: (1000 + 1000) / 1e6. opus $4/$20 per M:
    // (4000 + 2000) / 1e6 + (8000 + 2000) / 1e6.
    assert_eq!(haiku.model.as_deref(), Some(HAIKU));
    assert_eq!(haiku.tokens, [1000, 200, 0, 0]);
    assert!(close(0.002, haiku.cost_usd.expect("priced")));
    assert_eq!(opus.model.as_deref(), Some(OPUS));
    assert_eq!(opus.tokens, [3000, 200, 0, 0]);
    assert!(close(0.016, opus.cost_usd.expect("priced")));
    // Time stays whole, on the model with the most requests: 10:00-10:10,
    // waiting 10:02-10:04 and 10:06-10:08.
    let time = |c: &Cell| (c.wall_ms, c.active_ms, c.wait_ms);
    assert_eq!(time(haiku), (0, 0, 0));
    assert_eq!(time(opus), (10 * MIN, 10 * MIN, 4 * MIN));
}

#[test]
fn a_regroup_that_drops_a_window_leaves_no_cell_without_one() {
    let t = Temp::new("regroup");
    let mut store = Store::open_in(t.db(), TORONTO).expect("open");
    let main = vec![
        title("demo-9"),
        prompt("m1", "2026-10-03T10:00:00.000Z"),
        reply(
            "m2",
            Some("2026-10-03T10:05:00.000Z"),
            "req_m1",
            HAIKU,
            tokens(1, 1),
        ),
        title("demo-9-s2"),
        prompt("m3", "2026-10-03T10:10:00.000Z"),
        reply(
            "m4",
            Some("2026-10-03T10:15:00.000Z"),
            "req_m2",
            HAIKU,
            tokens(1, 1),
        ),
    ];
    store
        .ingest(&batch(&transcript(&t, "s1", None), main))
        .expect("ingest");
    // The second title stops counting: a regroup drops its window.
    Connection::open(t.db())
        .expect("db")
        .execute(
            "UPDATE events SET custom_title = NULL WHERE custom_title = 'demo-9-s2'",
            params![],
        )
        .expect("untitle");
    store.regroup_all().expect("regroup");
    let db = reader(&t);
    let segments: i64 = db
        .query_row("SELECT count(*) FROM segments", [], |r| r.get(0))
        .expect("segments");
    assert_eq!(segments, 1, "the regroup dropped a window");
    invariants(&db);
    let windows: Vec<Option<String>> = cells(&db, "s1").into_iter().map(|c| c.window).collect();
    assert_eq!(windows, vec![Some("demo-9".to_string())]);
}

#[test]
fn an_unknown_zone_is_refused() {
    let t = Temp::new("bad-zone");
    assert!(Store::open_in(t.db(), "Lorem/Ipsum").is_err());
}

#[test]
fn the_default_open_stores_the_system_zone() {
    let t = Temp::new("system-zone");
    drop(Store::open(t.db()).expect("open"));
    let tz: String = reader(&t)
        .query_row("SELECT value FROM meta WHERE key = 'tz'", [], |r| r.get(0))
        .expect("tz meta");
    assert!(
        jiff::tz::TimeZone::get(&tz).is_ok(),
        "{tz} is a zone jiff knows"
    );
}
