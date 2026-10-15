//! Performance inputs (#51) over the synthetic `tests/fixtures/perf-home`:
//! each dispatch's outcome from its task notifications and final report,
//! follow-ups by SendMessage, context peaks, and the API-error and
//! SendMessage columns of `events`, and the performance read over them
//! with the GitHub cache from `tests/fixtures/github/perf-home.json`; and a
//! background dispatch's state on the canvas from its launch to its
//! notification (#42). Every db lives in a temp dir.

use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use strate_core::cost::context_pct;
use strate_core::discovery::discover;
use strate_core::github::{Runner, refresh};
use strate_core::hooks::{Hook, HookEvent};
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{
    self, AgentState, AuditFilter, GroupBy, Performance, PermissionWaits, Tokens, performance,
};
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};
use strate_core::time::parse_timestamp;

const SID: &str = "5e55e1e1-0000-4000-8000-0000000000e1";
/// Window perf-1 on 2026-10-05: issue #1's earlier work.
const G2: &str = "5e55e1e1-0000-4000-8000-0000000000e2";
/// Window perf-3 on 2026-10-13: b7 (no report) and b8 (running).
const G3: &str = "5e55e1e1-0000-4000-8000-0000000000e3";
/// Never named, G3's /clear successor: Unlinked.
const G4: &str = "5e55e1e1-0000-4000-8000-0000000000e4";
const B: [&str; 6] = [
    "a6000000000000001",
    "a6000000000000002",
    "a6000000000000003",
    "a6000000000000004",
    "a6000000000000005",
    "a6000000000000006",
];

mod common;

use common::{Temp, batch, fixture_home, records, transcripts as transcripts_in};

fn root() -> PathBuf {
    fixture_home("perf-home")
}

/// Every transcript under `root/projects`, sorted: the orchestrator first.
fn transcripts() -> Vec<PathBuf> {
    transcripts_in(&root())
}

/// Lines `range` of `path` as one batch, each a record at its byte offset,
/// checkpointed at the end of the last: a transcript as it grows.
fn lines(path: &Path, range: Range<usize>) -> Batch {
    let (events, ends): (Vec<_>, Vec<_>) = records(path, range).into_iter().unzip();
    Batch {
        path: path.into(),
        events,
        checkpoint: Some(Checkpoint {
            offset: ends.last().copied().unwrap_or(0),
            identity: FileIdentity {
                volume: 0,
                index: 0,
            },
        }),
    }
}

/// The discovery graph, then each transcript as one batch in `order`.
fn build(t: &Temp, order: &[PathBuf]) -> Connection {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    for path in order {
        store.ingest(&batch(path)).expect("ingest");
    }
    drop(store);
    Connection::open(t.db()).expect("db")
}

/// An agent's (outcome, followups, context_peak_pct); `None` is the
/// orchestrator.
type Perf = (Option<String>, Option<i64>, Option<f64>);

fn perf(db: &Connection, agent: Option<&str>) -> Perf {
    db.query_row(
        "SELECT outcome, followups, context_peak_pct FROM agents
         WHERE session_id = ?1 AND agent_id IS ?2",
        params![SID, agent],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .expect("agent")
}

fn outcomes(db: &Connection) -> Vec<Option<String>> {
    B.iter().map(|b| perf(db, Some(b)).0).collect()
}

fn event_column(db: &Connection, column: &str, uuid: &str) -> Option<String> {
    db.query_row(
        &format!("SELECT {column} FROM events WHERE uuid = ?1"),
        [uuid],
        |r| r.get(0),
    )
    .optional()
    .expect(column)
    .flatten()
}

#[test]
fn an_outcome_is_the_worst_notified_status_and_a_needs_context_report_is_blocked() {
    let t = Temp::new("outcomes");
    let db = build(&t, &transcripts());
    let s = |v: &str| Some(v.to_string());
    // b1 completed four times over; b2 completed then failed; b3 killed; b4
    // completed with a NEEDS_CONTEXT: line in its handback; b5 completed,
    // quoting NEEDS_CONTEXT mid-line in its last text; b6 never notified.
    assert_eq!(
        outcomes(&db),
        vec![
            s("completed"),
            s("failed"),
            s("killed"),
            s("blocked"),
            s("completed"),
            None
        ]
    );
    assert_eq!(perf(&db, None).0, None, "the orchestrator has no outcome");
}

#[test]
fn notifications_are_read_from_their_carriers_only() {
    let t = Temp::new("carriers");
    let db = build(&t, &transcripts());
    let rows = |task: &str| -> Vec<(Option<String>, Option<String>)> {
        db.prepare(
            "SELECT type, notification_status FROM events
             WHERE notification_task = ?1 ORDER BY timestamp, byte_offset",
        )
        .expect("rows")
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    };
    let row = |kind: &str, status: &str| (Some(kind.to_string()), Some(status.to_string()));
    // Two queue operations, the queued_command attachment and the prompt.
    assert_eq!(
        rows(B[0]),
        vec![
            row("queue-operation", "completed"),
            row("queue-operation", "completed"),
            row("attachment", "completed"),
            row("user", "completed"),
        ]
    );
    // A typed-looking prompt with no origin is a notification by its text.
    assert_eq!(rows(B[2]), vec![row("user", "killed")]);
    // Quoted in a Bash command and its output: not a notification.
    assert_eq!(rows(B[5]), vec![]);
    // A background shell's is stored, and matches no agent.
    assert_eq!(rows("bshell001"), vec![row("queue-operation", "failed")]);
}

#[test]
fn followups_count_send_messages_by_agent_id_or_name() {
    let t = Temp::new("followups");
    let db = build(&t, &transcripts());
    // b1 by id from the orchestrator; b2 (perf-helper) by name from the
    // orchestrator and from b1; "nobody" matches no agent.
    let followups: Vec<Option<i64>> = B.iter().map(|b| perf(&db, Some(b)).1).collect();
    assert_eq!(
        followups,
        vec![Some(1), Some(2), Some(0), Some(0), Some(0), Some(0)]
    );
    assert_eq!(
        perf(&db, None).1,
        None,
        "the orchestrator is not followed up"
    );
    assert_eq!(
        event_column(&db, "message_to", "g-23").as_deref(),
        Some(r#"["perf-helper","nobody"]"#)
    );
    assert_eq!(
        event_column(&db, "message_to", "g-21").as_deref(),
        Some(r#"["a6000000000000001"]"#)
    );
    assert_eq!(event_column(&db, "message_to", "g-18"), None);
}

#[test]
fn the_context_peak_is_the_deepest_request_not_the_latest() {
    let t = Temp::new("peak");
    let db = build(&t, &transcripts());
    // b1: input 1000 + cache read 99000 = 100k, then 500 + 9500 = 10k.
    let sonnet = |depth| context_pct(depth, "claude-sonnet-5-5").expect("window");
    let (_, _, peak) = perf(&db, Some(B[0]));
    assert_eq!(peak, Some(sonnet(100_000)));
    let latest: f64 = db
        .query_row(
            "SELECT context_pct FROM agents WHERE agent_id = ?1",
            [B[0]],
            |r| r.get(0),
        )
        .expect("latest");
    assert_eq!(latest, sonnet(10_000));
    // Every orchestrator request is 1000 input + 10000 cache read.
    let opus = context_pct(11_000, "claude-opus-5-5").expect("window");
    assert_eq!(perf(&db, None).2, Some(opus));
}

#[test]
fn api_errors_split_into_prompt_too_long_and_other() {
    let t = Temp::new("api-errors");
    let db = build(&t, &transcripts());
    assert_eq!(
        event_column(&db, "api_error", "g-24").as_deref(),
        Some("prompt_too_long")
    );
    assert_eq!(
        event_column(&db, "api_error", "g-25").as_deref(),
        Some("other")
    );
    let flagged: i64 = db
        .query_row(
            "SELECT count(*) FROM events WHERE api_error IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(flagged, 2);
}

#[test]
fn any_ingest_order_converges() {
    let t = Temp::new("order-fwd");
    let forward = build(&t, &transcripts());
    let r = Temp::new("order-rev");
    let mut reversed: Vec<PathBuf> = transcripts();
    reversed.reverse();
    let backward = build(&r, &reversed);
    let all = |db: &Connection| -> Vec<Perf> {
        std::iter::once(None)
            .chain(B.iter().map(|b| Some(*b)))
            .map(|a| perf(db, a))
            .collect()
    };
    assert_eq!(all(&forward), all(&backward));
}

#[test]
fn opening_a_store_from_an_older_rollup_version_backfills_every_column() {
    let t = Temp::new("backfill");
    let db = build(&t, &transcripts());
    let want = outcomes(&db);
    db.execute_batch(
        "UPDATE events SET notification_task = NULL, notification_status = NULL,
             message_to = NULL, api_error = NULL;
         UPDATE agents SET outcome = NULL, followups = NULL, context_peak_pct = NULL;
         UPDATE meta SET value = '2' WHERE key = 'rollup';",
    )
    .expect("an older store");
    drop(db);
    drop(Store::open_in(t.db(), "UTC").expect("reopen"));
    let db = Connection::open(t.db()).expect("db");
    assert_eq!(outcomes(&db), want);
    assert_eq!(perf(&db, Some(B[1])).1, Some(2));
    assert_eq!(
        event_column(&db, "api_error", "g-24").as_deref(),
        Some("prompt_too_long")
    );
}

/// Each dispatch's state on perf-1's canvas, in `B` order.
fn states(reader: &Connection) -> Vec<AgentState> {
    let rows = views::workstreams(reader).expect("workstreams");
    let perf1 = rows.iter().find(|w| w.name == "perf-1").expect("perf-1");
    let graph = views::live_graph(reader, perf1.id)
        .expect("query")
        .expect("a graph");
    assert_eq!(graph.session_id, SID);
    B.iter()
        .map(|b| {
            let agent = graph
                .agents
                .iter()
                .find(|a| a.agent_id.as_deref() == Some(b));
            agent.unwrap_or_else(|| panic!("{b} not drawn")).state
        })
        .collect()
}

#[test]
fn a_background_dispatch_works_from_its_launch_until_its_notification() {
    use AgentState::{Done, Working};
    let t = Temp::new("background");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&root()).expect("discover"))
        .expect("graph");
    let entry = format!(
        r#"[{{"pid":4101,"cwd":"/work/perf","kind":"interactive","startedAt":1791000000000,
             "sessionId":"{SID}","name":"perf-1","status":"busy"}}]"#
    );
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: parse_snapshot(entry.as_bytes()).expect("entry").remove(0),
            at_ms: 1000,
        }])
        .expect("live");
    let reader = Store::open_reader(t.db()).expect("reader");
    let main = root().join(format!("projects/-work-perf/{SID}.jsonl"));
    let b1 = root().join(format!(
        "projects/-work-perf/{SID}/subagents/agent-{}.jsonl",
        B[0]
    ));

    // The title, the prompt, b1's dispatch and its immediate launch result.
    store.ingest(&lines(&main, 0..4)).expect("launch");
    assert_eq!(states(&reader)[0], Working, "a launch result is no answer");
    // b1's transcript grows: its task, then two requests.
    for n in 0..3 {
        store.ingest(&lines(&b1, n..n + 1)).expect("b1 grows");
        assert_eq!(states(&reader)[0], Working, "b1 record {n}");
    }
    // The other launches, then every notification: b6 is never notified.
    store
        .ingest(&lines(&main, 4..usize::MAX))
        .expect("notifications");
    assert_eq!(
        states(&reader),
        [Done, Done, Done, Done, Done, Working],
        "a notification of any status ends a dispatch"
    );
    // b6's SubagentStop hook ends it too.
    store
        .apply_hook(&Hook {
            received_ms: 2000,
            event: HookEvent::SubagentStop {
                session_id: SID.into(),
                agent_id: B[5].into(),
                agent_type: Some("implementer".into()),
            },
        })
        .expect("stop");
    assert_eq!(states(&reader), [Done; 6]);
}

/// The store built from every transcript, with `perf`'s GitHub cache
/// (`gh api user`, then the issues query).
fn built(t: &Temp) -> Connection {
    drop(build(t, &transcripts()));
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github");
    let mut script = ["user.json", "perf-home.json"]
        .map(|f| dir.join(f))
        .into_iter();
    let mut runner: Runner = Box::new(move |_| match script.next() {
        Some(path) => fs::read(path),
        None => Err(io::Error::other("script exhausted")),
    });
    refresh(&mut store, &mut runner, &["perf"], 1).expect("gh refresh");
    drop(store);
    Connection::open(t.db()).expect("db")
}

fn filter(from: &str, to: &str, repos: &[&str]) -> AuditFilter {
    AuditFilter {
        from: from.into(),
        to: to.into(),
        repos: repos.iter().map(|r| r.to_string()).collect(),
        group_by: GroupBy::Hierarchy,
    }
}

/// The 2026-10-06..13 read, over every repo or `repos`.
fn read(db: &Connection, repos: &[&str]) -> Performance {
    performance(db, &filter("2026-10-06", "2026-10-13", repos)).expect("performance")
}

/// `total(column)` over the `audit_cells` rows matching `filter` (SQL).
fn cells_total(db: &Connection, column: &str, filter: &str) -> f64 {
    db.query_row(
        &format!("SELECT total({column}) FROM audit_cells WHERE {filter}"),
        [],
        |r| r.get(0),
    )
    .expect(column)
}

fn usd(db: &Connection, filter: &str) -> f64 {
    cells_total(db, "cost_usd", filter)
}

fn ms(db: &Connection, column: &str, filter: &str) -> i64 {
    cells_total(db, column, filter) as i64
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn sessions(ids: &[&str]) -> String {
    let quoted: Vec<String> = ids.iter().map(|id| format!("'{id}'")).collect();
    format!("session_id IN ({})", quoted.join(", "))
}

#[test]
fn a_closed_issue_counts_in_its_close_week_at_its_whole_life_cost() {
    let t = Temp::new("outcome");
    let db = built(&t);
    let o = read(&db, &[]).outcome;
    let perf1 = sessions(&[SID, G2]);
    let life = usd(&db, &perf1);
    let oct6 = usd(&db, &sessions(&[SID]));
    assert!(
        life > oct6,
        "G2's 10-05 work is outside the range but in the cost"
    );

    let weeks: Vec<(&str, i64)> = o.weeks.iter().map(|w| (w.week.as_str(), w.days)).collect();
    assert_eq!(weeks, [("2026-10-05", 6), ("2026-10-12", 2)]);
    let w = &o.weeks[0];
    assert_eq!((w.issues_closed, w.prs_merged), (1, 1));
    assert!(w.usd_per_closed_issue.is_some_and(|u| close(u, life)));
    assert!(w.usd_per_merged_pr.is_some_and(|u| close(u, life)));
    assert_eq!(
        w.active_ms_per_closed_issue,
        Some(ms(
            &db,
            "active_ms",
            &format!("agent_id IS NULL AND {perf1}")
        )),
        "orchestrator time only"
    );
    assert!(close(w.spend_usd, oct6));
    let w = &o.weeks[1];
    assert_eq!((w.issues_closed, w.prs_merged), (0, 0));
    assert_eq!(w.usd_per_closed_issue, None);
    assert_eq!(w.active_ms_per_closed_issue, None);
    assert_eq!(w.usd_per_merged_pr, None);
    assert!(close(w.spend_usd, usd(&db, &sessions(&[G3, G4]))));

    assert!(close(o.unlinked_usd, usd(&db, &sessions(&[G4]))));
    assert_eq!(o.unlinked_workstreams, 1);
    assert!(close(o.open_usd, usd(&db, &sessions(&[G3]))));
    assert_eq!(o.open_issues, 1);

    // Closed on 10-06: a range that ends before it has no closed issue.
    let before = performance(&db, &filter("2026-10-01", "2026-10-05", &[])).expect("read");
    assert!(before.outcome.weeks.iter().all(|w| w.issues_closed == 0));
}

#[test]
fn agents_roll_up_per_type_and_model_with_orchestrator_rows_flagged() {
    let t = Temp::new("agents");
    let db = built(&t);
    let p = read(&db, &[]);
    let rows: Vec<(&str, &str, bool)> = p
        .agents
        .iter()
        .map(|a| (a.agent_type.as_str(), a.model.as_str(), a.orchestrator))
        .collect();
    assert_eq!(
        rows,
        [
            ("orchestrator", "claude-opus-5-5", true),
            ("implementer", "claude-sonnet-5-5", false),
        ]
    );
    let range = "day BETWEEN '2026-10-06' AND '2026-10-13'";
    let o = &p.agents[0];
    assert_eq!(
        (o.dispatches, o.completed, o.followups, o.usd_per_dispatch),
        (0, 0, 0, None)
    );
    assert!(close(
        o.cost_usd,
        usd(&db, &format!("agent_id IS NULL AND {range}"))
    ));
    // Every orchestrator request reads 10000 cached of 11000 input tokens.
    assert!(
        o.cache_hit_pct
            .is_some_and(|h| close(h, 100.0 * 10.0 / 11.0))
    );
    let i = &p.agents[1];
    // b1 b5 completed, b4 blocked, b2 failed, b3 killed, b6 b7 never
    // notified, b8 still running.
    assert_eq!(
        (
            i.dispatches,
            i.completed,
            i.blocked,
            i.failed,
            i.no_report,
            i.running
        ),
        (8, 2, 1, 2, 2, 1)
    );
    assert_eq!(i.followups, 3);
    let subagents = format!("agent_id IS NOT NULL AND {range}");
    let sub = usd(&db, &subagents);
    assert!(close(i.cost_usd, sub));
    assert!(i.usd_per_dispatch.is_some_and(|u| close(u, sub / 8.0)));
    let tokens = |column: &str| ms(&db, column, &subagents);
    assert_eq!(
        i.tokens,
        Tokens {
            input: tokens("input_tokens"),
            output: tokens("output_tokens"),
            cache_read: tokens("cache_read_tokens"),
            cache_write: tokens("cache_write_tokens"),
        }
    );
}

#[test]
fn the_bottleneck_splits_your_time_per_day_and_repo_with_a_blocked_proxy() {
    let t = Temp::new("bottleneck");
    let db = built(&t);
    let b = read(&db, &[]).bottleneck;
    // No hook ever recorded a permission prompt.
    assert_eq!(
        b.permission_waits,
        PermissionWaits {
            recorded_since: None,
            open: 0
        }
    );
    assert!(
        b.days
            .iter()
            .all(|d| d.permission_ms == 0 && d.permission_prompts == 0)
    );
    let keys: Vec<(&str, Option<&str>)> = b
        .days
        .iter()
        .map(|d| (d.day.as_str(), d.repo.as_deref()))
        .collect();
    assert_eq!(
        keys,
        [
            ("2026-10-06", Some("perf")),
            ("2026-10-13", Some("perf")),
            ("2026-10-13", None),
        ]
    );
    let oct6 = &b.days[0];
    let main = format!("agent_id IS NULL AND session_id = '{SID}' AND day = '2026-10-06'");
    // The AskUserQuestion 10:13:00 to its answer 10:14:00.
    assert_eq!(oct6.ask_ms, 60_000);
    assert_eq!(
        oct6.prompt_wait_ms,
        ms(&db, "wait_ms", &main) - ms(&db, "ask_ms", &main)
    );
    assert_eq!(oct6.active_ms, ms(&db, "active_ms", &main));
    // b4's last record 10:01:05 to the orchestrator's next dispatch, its
    // SendMessage at 10:10:00 (b4's own notification is no answer).
    assert_eq!((oct6.blocked_ms, oct6.blocked_reports), (535_000, 1));
    assert!(
        b.days[1..]
            .iter()
            .all(|d| d.blocked_ms == 0 && d.blocked_reports == 0)
    );
}

#[test]
fn waste_counts_rework_and_errors_in_the_range() {
    let t = Temp::new("waste");
    let db = built(&t);
    let w = read(&db, &[]).waste;
    // SendMessage: to b1 by id, to perf-helper twice, to nobody.
    assert_eq!((w.followups, w.send_messages), (3, 4));
    assert_eq!((w.dispatches, w.blocked, w.no_report), (8, 1, 2));
    let top = w.no_report_top.expect("a top no-report lane");
    assert_eq!(
        (top.agent_type.as_str(), top.model.as_str(), top.count),
        ("implementer", "claude-sonnet-5-5", 2)
    );
    assert_eq!((w.compactions, w.clears), (1, 1));
    let deepest = w.deepest_peak.expect("a peak");
    let b8 = context_pct(850_000, "claude-sonnet-5-5").expect("window");
    assert_eq!(deepest.model, "claude-sonnet-5-5");
    assert!(close(deepest.pct, b8));
    assert_eq!(w.context_peaks_over_80, 1);
    assert_eq!((w.api_retries, w.api_errors, w.prompt_too_long), (1, 2, 1));
    assert_eq!((w.reopened_issues, w.reverted_prs, w.repos), (1, 1, 1));

    // From 10-07: none of the 10-06 work, so no follow-up, compaction, API
    // error, reopened issue (#1's work is all before) or revert (merged
    // 10-06); G3's retry and G4's /clear remain.
    let later = performance(&db, &filter("2026-10-07", "2026-10-13", &[]))
        .expect("read")
        .waste;
    assert_eq!(
        (later.followups, later.send_messages, later.compactions),
        (0, 0, 0)
    );
    assert_eq!(
        (later.api_errors, later.api_retries, later.clears),
        (0, 1, 1)
    );
    assert_eq!(
        (later.reopened_issues, later.reverted_prs, later.dispatches),
        (0, 0, 2)
    );
}

#[test]
fn the_repo_filter_applies_to_every_section() {
    let t = Temp::new("repos");
    let db = built(&t);
    let p = read(&db, &["perf"]);
    assert_eq!(
        (p.outcome.unlinked_usd, p.outcome.unlinked_workstreams),
        (0.0, 0)
    );
    assert!(close(
        p.outcome.weeks[1].spend_usd,
        usd(&db, &sessions(&[G3]))
    ));
    assert!(
        p.bottleneck
            .days
            .iter()
            .all(|d| d.repo.as_deref() == Some("perf"))
    );
    assert_eq!(p.waste.clears, 0, "the /clear successor is Unlinked");
    assert_eq!(p.waste.repos, 1);

    let none = read(&db, &["other"]);
    assert!(
        none.outcome
            .weeks
            .iter()
            .all(|w| w.spend_usd == 0.0 && w.issues_closed == 0)
    );
    assert_eq!((none.outcome.open_issues, none.outcome.open_usd), (0, 0.0));
    assert!(none.agents.is_empty() && none.bottleneck.days.is_empty());
    let w = &none.waste;
    assert_eq!(
        (w.dispatches, w.send_messages, w.compactions, w.api_retries),
        (0, 0, 0, 0)
    );
    assert_eq!((w.reopened_issues, w.reverted_prs, w.repos), (0, 0, 0));
    assert!(w.deepest_peak.is_none() && w.no_report_top.is_none());
}

#[test]
fn performance_serializes_in_camel_case_and_a_bad_day_is_an_error() {
    let t = Temp::new("json");
    let db = built(&t);
    let json = serde_json::to_value(read(&db, &[])).expect("json");
    assert_eq!(
        json["bottleneck"]["permissionWaits"],
        serde_json::json!({ "recordedSince": null, "open": 0 })
    );
    assert_eq!(json["bottleneck"]["days"][0]["permissionMs"], 0);
    assert_eq!(json["bottleneck"]["days"][0]["permissionPrompts"], 0);
    assert!(json["outcome"]["weeks"][0]["usdPerClosedIssue"].is_number());
    assert_eq!(json["waste"]["noReportTop"]["agentType"], "implementer");
    assert!(json["agents"][1]["cacheHitPct"].is_number());
    assert_eq!(json["waste"]["contextPeaksOver80"], 1);
    assert!(performance(&db, &filter("October", "2026-10-13", &[])).is_err());
}

fn at(iso: &str) -> i64 {
    parse_timestamp(iso).expect("timestamp")
}

fn permission(iso: &str, session: &str, agent_id: Option<&str>) -> Hook {
    asking(iso, session, agent_id, "Bash")
}

fn asking(iso: &str, session: &str, agent_id: Option<&str>, tool: &str) -> Hook {
    Hook {
        received_ms: at(iso),
        event: HookEvent::PermissionRequest {
            session_id: session.into(),
            agent_id: agent_id.map(Into::into),
            tool_name: Some(tool.into()),
        },
    }
}

/// The registry listing `session` busy at `iso`: its orchestrator's
/// permission prompt is answered.
fn busy(iso: &str, session: &str, pid: u32) -> RegistryEvent {
    let entry = parse_snapshot(
        format!(
            r#"[{{"pid":{pid},"cwd":"/work/perf","kind":"interactive","startedAt":1791000000000,
                "sessionId":"{session}","status":"busy"}}]"#
        )
        .as_bytes(),
    )
    .expect("entry")
    .remove(0);
    RegistryEvent::Appeared {
        entry,
        at_ms: at(iso),
    }
}

/// (day, repo, permission ms, permission prompts) of each bottleneck row.
fn permission_rows(p: &Performance) -> Vec<(String, Option<String>, i64, i64)> {
    p.bottleneck
        .days
        .iter()
        .map(|d| {
            (
                d.day.clone(),
                d.repo.clone(),
                d.permission_ms,
                d.permission_prompts,
            )
        })
        .collect()
}

fn row(day: &str, repo: Option<&str>, ms: i64, prompts: i64) -> (String, Option<String>, i64, i64) {
    (day.into(), repo.map(Into::into), ms, prompts)
}

#[test]
fn permission_waits_count_on_the_day_they_started_in_their_window_repo() {
    let t = Temp::new("permission");
    drop(built(&t));
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    // G2 (10-05, before the range): 10 s.
    store
        .apply_hook(&permission("2026-10-05T09:00:10.000Z", G2, None))
        .expect("g2");
    store
        .apply_registry(&[busy("2026-10-05T09:00:20.000Z", G2, 4102)])
        .expect("g2 busy");
    // The orchestrator: 2 min; its dispatch b1: 15 s, until it stopped.
    store
        .apply_hook(&permission("2026-10-06T10:05:00.000Z", SID, None))
        .expect("sid");
    store
        .apply_registry(&[busy("2026-10-06T10:07:00.000Z", SID, 4101)])
        .expect("sid busy");
    store
        .apply_hook(&permission("2026-10-06T10:00:30.000Z", SID, Some(B[0])))
        .expect("b1");
    store
        .apply_hook(&Hook {
            received_ms: at("2026-10-06T10:00:45.000Z"),
            event: HookEvent::SubagentStop {
                session_id: SID.into(),
                agent_id: B[0].into(),
                agent_type: None,
            },
        })
        .expect("b1 stop");
    // Questions and plan approvals are ask time already: b2's answered
    // AskUserQuestion and b3's open ExitPlanMode count nowhere here.
    store
        .apply_hook(&asking(
            "2026-10-06T10:02:00.000Z",
            SID,
            Some(B[1]),
            "AskUserQuestion",
        ))
        .expect("b2 asks");
    store
        .apply_hook(&Hook {
            received_ms: at("2026-10-06T10:09:00.000Z"),
            event: HookEvent::SubagentStop {
                session_id: SID.into(),
                agent_id: B[1].into(),
                agent_type: None,
            },
        })
        .expect("b2 stop");
    store
        .apply_hook(&asking(
            "2026-10-06T10:03:00.000Z",
            SID,
            Some(B[2]),
            "ExitPlanMode",
        ))
        .expect("b3 plans");
    // G3 (perf-3) still waiting: its request comes after both links'
    // transcripts were stored and no batch follows, so neither its own
    // reply (10:20) nor G4's first record (10:30) ends it (#78). G4
    // (Unlinked) 30 s.
    store
        .apply_hook(&permission("2026-10-13T10:00:00.000Z", G3, None))
        .expect("g3");
    store
        .apply_hook(&permission("2026-10-13T10:30:10.000Z", G4, None))
        .expect("g4");
    store
        .apply_registry(&[busy("2026-10-13T10:30:40.000Z", G4, 4104)])
        .expect("g4 busy");
    drop(store);
    let db = Connection::open(t.db()).expect("db");

    let all = read(&db, &[]);
    assert_eq!(
        permission_rows(&all),
        [
            row("2026-10-06", Some("perf"), 135_000, 2),
            row("2026-10-13", Some("perf"), 0, 1),
            row("2026-10-13", None, 30_000, 1),
        ]
    );
    let since = Some("2026-10-05".to_string());
    assert_eq!(
        all.bottleneck.permission_waits,
        PermissionWaits {
            recorded_since: since.clone(),
            open: 1
        }
    );

    let perf = read(&db, &["perf"]);
    assert_eq!(
        permission_rows(&perf),
        [
            row("2026-10-06", Some("perf"), 135_000, 2),
            row("2026-10-13", Some("perf"), 0, 1),
        ]
    );
    assert_eq!(perf.bottleneck.permission_waits.open, 1);

    let other = read(&db, &["other"]);
    assert!(other.bottleneck.days.is_empty());
    assert_eq!(
        other.bottleneck.permission_waits,
        PermissionWaits {
            recorded_since: since,
            open: 0
        }
    );

    let later = performance(&db, &filter("2026-10-07", "2026-10-13", &[])).expect("read");
    assert!(later.bottleneck.days.iter().all(|d| d.day == "2026-10-13"));
}

#[test]
fn an_orchestrator_wait_counts_in_the_window_it_started_in() {
    let t = Temp::new("permission-windows");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    let path: Arc<Path> = Path::new("/cfg/projects/-work-x/s-rename.jsonl").into();
    let records = [
        r#"{"type":"custom-title","customTitle":"alpha-1"}"#,
        r#"{"type":"user","uuid":"u1","timestamp":"2026-10-07T10:00:00.000Z","message":{"content":"Lorem."}}"#,
        r#"{"type":"assistant","uuid":"u2","timestamp":"2026-10-07T10:00:05.000Z","message":{"content":[]}}"#,
        r#"{"type":"custom-title","customTitle":"beta-2"}"#,
        r#"{"type":"user","uuid":"u3","timestamp":"2026-10-07T11:00:00.000Z","message":{"content":"Ipsum."}}"#,
        r#"{"type":"assistant","uuid":"u4","timestamp":"2026-10-07T11:00:05.000Z","message":{"content":[]}}"#,
    ];
    let part = |range: Range<usize>| Batch {
        path: path.clone(),
        events: range
            .map(|i| TailEvent::Record {
                path: path.clone(),
                offset: i as u64 * 200,
                value: serde_json::from_str(records[i]).expect("json"),
            })
            .collect(),
        checkpoint: None,
    };
    // Each prompt is answered by the window's next record.
    store
        .apply_hook(&permission("2026-10-07T10:00:02.000Z", "s-rename", None))
        .expect("alpha");
    store.ingest(&part(0..3)).expect("alpha window");
    store
        .apply_hook(&permission("2026-10-07T11:00:01.000Z", "s-rename", None))
        .expect("beta");
    store.ingest(&part(3..6)).expect("beta window");
    drop(store);
    let db = Connection::open(t.db()).expect("db");
    let p = performance(&db, &filter("2026-10-07", "2026-10-07", &[])).expect("read");
    assert_eq!(
        permission_rows(&p),
        [
            row("2026-10-07", Some("alpha"), 3_000, 1),
            row("2026-10-07", Some("beta"), 4_000, 1),
        ]
    );
}
