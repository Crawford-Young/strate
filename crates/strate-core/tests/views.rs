//! The read views the app's commands serve, over a store filled with
//! synthetic transcripts, a discovery graph, registry polls and hooks.
//! Every db lives in a temp dir.

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use serde_json::{Value, json};
use strate_core::discovery::{AgentRef, Graph, Link, SubagentMeta, SubagentNode, discover};
use strate_core::hooks::{Hook, HookEvent};
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{self, AgentState, GraphAgent, GraphEdge, Workstream};
use strate_core::tail::{Batch, TailEvent};

mod common;

use common::{Temp, fixture_home, records, transcripts};

const PROJECT: &str = "/cfg/projects/-work-alpha";

fn path(rel: &str) -> Arc<Path> {
    Path::new(PROJECT).join(rel).into()
}

/// One tail batch holding `records` for the transcript at `rel`. Unlike
/// `common::batch` its records are synthetic, 100 bytes apart, and it
/// carries no checkpoint.
fn batch(rel: &str, records: &[Value]) -> Batch {
    let path = path(rel);
    let events = records
        .iter()
        .enumerate()
        .map(|(i, value)| TailEvent::Record {
            path: path.clone(),
            offset: i as u64 * 100,
            value: value.clone(),
        })
        .collect();
    Batch {
        path,
        events,
        checkpoint: None,
    }
}

fn title(name: &str) -> Value {
    json!({"type": "custom-title", "customTitle": name})
}

fn prompt(uuid: &str, at: &str) -> Value {
    json!({"type": "user", "uuid": uuid, "timestamp": at, "cwd": "/work/alpha",
           "message": {"role": "user", "content": "Lorem ipsum."}})
}

/// `prompt`, written by a process whose first session is `root`.
fn rooted(uuid: &str, at: &str, root: &str) -> Value {
    let mut value = prompt(uuid, at);
    value["session_id"] = json!(root);
    value
}

/// An assistant request on haiku: `input` fresh plus `cached` read tokens,
/// with a `tool_use` block per id in `dispatches`.
fn reply(uuid: &str, at: &str, input: u64, cached: u64, dispatches: &[&str]) -> Value {
    let content: Vec<Value> = dispatches
        .iter()
        .map(|id| json!({"type": "tool_use", "id": id, "name": "Agent", "input": {}}))
        .collect();
    json!({"type": "assistant", "uuid": uuid, "timestamp": at, "requestId": format!("req-{uuid}"),
           "message": {"id": format!("msg-{uuid}"), "model": "claude-haiku-4-5", "content": content,
                       "usage": {"input_tokens": input, "cache_read_input_tokens": cached,
                                 "output_tokens": 100}}})
}

/// A foreground dispatch's answer: its subagent's report.
fn result(uuid: &str, at: &str, tool_use_id: &str) -> Value {
    json!({"type": "user", "uuid": uuid, "timestamp": at,
           "message": {"role": "user", "content": [
               {"type": "tool_result", "tool_use_id": tool_use_id, "content": "Lorem."}]},
           "toolUseResult": {"status": "completed", "content": [{"type": "text", "text": "Lorem."}]}})
}

fn dispatched(session: &str, agent: &str, tool_use_id: &str) -> SubagentNode {
    SubagentNode {
        session_id: session.into(),
        agent_id: agent.into(),
        path: Path::new(PROJECT).join(format!("{session}/subagents/agent-{agent}.jsonl")),
        meta: Some(SubagentMeta {
            agent_type: Some("implementer".into()),
            description: Some(format!("Lorem {agent}")),
            tool_use_id: Some(tool_use_id.into()),
            ..SubagentMeta::default()
        }),
        link: Link::Dispatch {
            parent: AgentRef::Session {
                session_id: session.into(),
            },
            tool_use_id: tool_use_id.into(),
        },
    }
}

fn appeared(json: &str) -> RegistryEvent {
    RegistryEvent::Appeared {
        entry: parse_snapshot(format!("[{json}]").as_bytes())
            .expect("entry")
            .remove(0),
        at_ms: 5000,
    }
}

/// Two processes, so two tabs. `alpha-1` ran `s-old` (with subagent `a0`),
/// then /cleared into the live `s-alpha` (busy, subagent `a1` done, `a2`
/// asking permission). `beta-2` is one session waiting on the user.
fn fill(store: &mut Store) {
    store
        .ingest(&batch(
            "s-old.jsonl",
            &[
                title("alpha-1"),
                rooted("o1", "2026-10-07T09:00:00.000Z", "s-old"),
                reply("o2", "2026-10-07T09:00:10.000Z", 10, 0, &["toolu_0"]),
                result("o3", "2026-10-07T09:00:30.000Z", "toolu_0"),
            ],
        ))
        .expect("old session");
    store
        .ingest(&batch(
            "s-old/subagents/agent-a0.jsonl",
            &[reply("o4", "2026-10-07T09:00:20.000Z", 10, 0, &[])],
        ))
        .expect("a0");
    store
        .ingest(&batch(
            "s-alpha.jsonl",
            &[
                title("alpha-1"),
                rooted("u1", "2026-10-08T10:00:00.000Z", "s-old"),
                // Latest depth 1000 + 49000 = 50000 of haiku's 200000: 25%.
                reply(
                    "u2",
                    "2026-10-08T10:00:10.000Z",
                    1000,
                    49_000,
                    &["toolu_1", "toolu_2"],
                ),
                result("u3", "2026-10-08T10:00:40.000Z", "toolu_1"),
            ],
        ))
        .expect("alpha session");
    for (agent, uuid, at) in [
        ("a1", "u4", "2026-10-08T10:00:20.000Z"),
        ("a2", "u5", "2026-10-08T10:00:21.000Z"),
    ] {
        store
            .ingest(&batch(
                &format!("s-alpha/subagents/agent-{agent}.jsonl"),
                &[reply(uuid, at, 2000, 0, &[])],
            ))
            .expect("subagent");
    }
    store
        .ingest(&batch(
            "s-beta.jsonl",
            &[
                title("beta-2"),
                prompt("b1", "2026-10-08T11:00:00.000Z"),
                reply("b2", "2026-10-08T11:00:05.000Z", 10, 0, &[]),
            ],
        ))
        .expect("beta session");
    store
        .ingest_graph(&Graph {
            subagents: vec![
                dispatched("s-old", "a0", "toolu_0"),
                dispatched("s-alpha", "a1", "toolu_1"),
                dispatched("s-alpha", "a2", "toolu_2"),
            ],
            ..Graph::default()
        })
        .expect("graph");
    store
        .apply_registry(&[
            appeared(
                r#"{"pid":4101,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
                    "sessionId":"s-alpha","name":"alpha-1","status":"busy"}"#,
            ),
            appeared(
                r#"{"pid":4102,"cwd":"/work/beta","kind":"interactive","startedAt":1791000000000,
                    "sessionId":"s-beta","name":"beta-2","status":"waiting","waitingFor":"permission"}"#,
            ),
        ])
        .expect("registry");
    store
        .apply_hook(&Hook {
            received_ms: 6000,
            event: HookEvent::PermissionRequest {
                session_id: "s-alpha".into(),
                agent_id: Some("a2".into()),
                tool_name: Some("Bash".into()),
            },
        })
        .expect("hook");
}

/// The tabs' names: their drawn links' charge keys.
const ALPHA: &str = "issue_alpha-1";
const BETA: &str = "issue_beta-2";

fn by_name<'a>(rows: &'a [Workstream], name: &str) -> &'a Workstream {
    rows.iter()
        .find(|w| w.name == name)
        .unwrap_or_else(|| panic!("no tab {name}: {rows:?}"))
}

fn cost(model_input: u64, cached: u64) -> f64 {
    // haiku: $1 / Mtok input, $0.10 / Mtok cache read, $5 / Mtok output.
    (model_input as f64 * 1.0 + cached as f64 * 0.1 + 100.0 * 5.0) / 1e6
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-12)
}

#[test]
fn tabs_carry_cost_state_needs_you_and_the_session_to_draw() {
    let temp = Temp::new("workstreams");
    let (mut store, reader) = temp.open();
    fill(&mut store);

    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(rows.len(), 2, "{rows:?}");
    let alpha = by_name(&rows, ALPHA);
    let beta = by_name(&rows, BETA);
    // Live first.
    assert_eq!(rows[0].name, BETA, "the latest live session leads");

    assert!(!alpha.fallback);
    // The chain's tab is its first link's orchestrator.
    let s_old: i64 = reader
        .query_row(
            "SELECT id FROM agents WHERE session_id = 's-old' AND agent_id IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("s-old");
    assert_eq!(alpha.id, s_old);
    // s-old (2 requests) + s-alpha (1) + a1 + a2.
    let alpha_usd = 2.0 * cost(10, 0) + cost(1000, 49_000) + 2.0 * cost(2000, 0);
    assert!(close(alpha.cost_usd, alpha_usd), "{:?}", alpha.cost_usd);
    assert_eq!(alpha.unpriced, 0);
    // a2's PermissionRequest.
    assert_eq!(alpha.needs_you, 1);
    assert_eq!(alpha.state, AgentState::NeedsYou);
    assert_eq!(alpha.session_id.as_deref(), Some("s-alpha"));
    assert!(alpha.live);
    assert_eq!(
        alpha.started_at.as_deref(),
        Some("2026-10-08T10:00:00.000Z")
    );

    assert_eq!(beta.state, AgentState::NeedsYou);
    assert_eq!(beta.needs_you, 1);
    assert!(close(beta.cost_usd, cost(10, 0)));

    let costs = views::workstream_costs(&reader).expect("costs");
    assert_eq!(costs.len(), 2);
    let alpha_cost = costs
        .iter()
        .find(|c| c.workstream == alpha.id)
        .expect("alpha $");
    assert!(close(alpha_cost.cost_usd, alpha_usd));
}

#[test]
fn needs_you_rows_name_their_tab() {
    let temp = Temp::new("needs-you");
    let (mut store, reader) = temp.open();
    fill(&mut store);
    let rows = views::workstreams(&reader).expect("workstreams");
    let (alpha, beta) = (by_name(&rows, ALPHA).id, by_name(&rows, BETA).id);

    let needs = views::needs_you(&reader).expect("needs_you");
    let got: Vec<_> = needs
        .iter()
        .map(|n| {
            (
                n.source.as_str(),
                n.session_id.as_deref(),
                n.agent_id.as_deref(),
                n.waiting_for.as_deref(),
                n.workstream,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "registry",
                Some("s-beta"),
                None,
                Some("permission"),
                Some(beta)
            ),
            (
                "hook",
                Some("s-alpha"),
                Some("a2"),
                Some("Bash"),
                Some(alpha)
            ),
        ]
    );
    assert_eq!(needs[0].since_ms, Some(5000));
}

#[test]
fn the_live_graph_is_the_live_session_only_with_states_and_context() {
    let temp = Temp::new("live-graph");
    let (mut store, reader) = temp.open();
    fill(&mut store);
    let rows = views::workstreams(&reader).expect("workstreams");
    let alpha = by_name(&rows, ALPHA).id;

    let graph = views::live_graph(&reader, alpha)
        .expect("query")
        .expect("a graph");
    assert_eq!(graph.session_id, "s-alpha");
    assert!(graph.live);
    let ids: Vec<Option<&str>> = graph.agents.iter().map(|a| a.agent_id.as_deref()).collect();
    // Never s-old's orchestrator or a0. a1's dispatch has its tool_result:
    // a done worker is left to the Audit (#81).
    assert_eq!(ids, [None, Some("a2")]);

    let [orchestrator, a2]: &[GraphAgent; 2] =
        graph.agents.as_slice().try_into().expect("two agents");
    assert_eq!(orchestrator.kind, "orchestrator");
    assert_eq!(orchestrator.name.as_deref(), Some("alpha-1"));
    assert_eq!(orchestrator.state, AgentState::Working);
    assert_eq!(orchestrator.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(orchestrator.context_tokens, Some(50_000));
    assert!(close(orchestrator.context_pct, 25.0));
    assert!(close(orchestrator.cost_usd, cost(1000, 49_000)));
    assert_eq!(orchestrator.parent, None);
    // The header sums the drawn link and the earlier s-old, with their
    // subagents; each link's run spans its first record to its last.
    let alpha_session_usd = 2.0 * cost(10, 0) + cost(1000, 49_000) + 2.0 * cost(2000, 0);
    assert!(
        close(graph.session_cost_usd, alpha_session_usd),
        "{:?}",
        graph.session_cost_usd
    );
    assert_eq!(graph.run_ms, Some(30_000 + 40_000));

    // a2 asked for permission.
    assert_eq!(a2.state, AgentState::NeedsYou);
    assert_eq!(a2.agent_type.as_deref(), Some("implementer"));
    assert_eq!(a2.description.as_deref(), Some("Lorem a2"));
    assert_eq!(a2.parent, Some(orchestrator.id));
    assert!(close(a2.context_pct, 1.0));

    assert_eq!(
        graph.edges,
        [GraphEdge {
            kind: "dispatch".into(),
            parent: orchestrator.id,
            child: a2.id,
        }]
    );
}

#[test]
fn a_tab_with_no_live_link_draws_its_latest_link_as_done() {
    let temp = Temp::new("ended");
    let (mut store, reader) = temp.open();
    fill(&mut store);
    let alpha_entry = r#"{"pid":4101,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
        "sessionId":"s-alpha","name":"alpha-1","status":"busy"}"#;
    let RegistryEvent::Appeared { entry, .. } = appeared(alpha_entry) else {
        unreachable!()
    };
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry,
            last_seen_ms: 7000,
        }])
        .expect("gone");
    // The PermissionRequest is answered by a2's next record.
    store
        .ingest(&batch(
            "s-alpha/subagents/agent-a2.jsonl",
            &[
                reply("u5", "2026-10-08T10:00:21.000Z", 2000, 0, &[]),
                reply("u6", "2026-10-08T10:00:50.000Z", 2000, 0, &[]),
            ],
        ))
        .expect("a2 moves on");

    let rows = views::workstreams(&reader).expect("workstreams");
    let alpha = by_name(&rows, ALPHA);
    assert!(!alpha.live);
    assert_eq!(alpha.state, AgentState::Done);
    assert_eq!(alpha.needs_you, 0);
    assert_eq!(alpha.session_id.as_deref(), Some("s-alpha"));

    let graph = views::live_graph(&reader, alpha.id)
        .expect("query")
        .expect("a graph");
    assert!(!graph.live);
    // Its workers read done, so only the orchestrator is drawn.
    let drawn: Vec<(Option<&str>, AgentState)> = graph
        .agents
        .iter()
        .map(|a| (a.agent_id.as_deref(), a.state))
        .collect();
    assert_eq!(drawn, [(None, AgentState::Done)]);
    assert!(graph.edges.is_empty());
}

#[test]
fn stale_and_finished_registry_entries_are_not_live() {
    let temp = Temp::new("not-live");
    let (mut store, reader) = temp.open();
    fill(&mut store);
    // A new run's first poll lists beta-2 and a finished background run in
    // alpha's session; alpha-1's process ended while nothing polled.
    let finished = appeared(
        r#"{"id":"bg-9","cwd":"/work/alpha","kind":"background","startedAt":1791000003000,
            "sessionId":"s-alpha","name":"alpha-1","state":"done"}"#,
    );
    store
        .apply_registry(&[
            finished,
            RegistryEvent::Baseline {
                keys: vec!["id:bg-9".into(), "pid:4102:1791000000000".into()],
            },
        ])
        .expect("first poll");

    let rows = views::workstreams(&reader).expect("workstreams");
    let alpha = by_name(&rows, ALPHA);
    assert!(!alpha.live);
    assert!(by_name(&rows, BETA).live);
    let graph = views::live_graph(&reader, alpha.id)
        .expect("query")
        .expect("a graph");
    assert!(!graph.live);
}

#[test]
fn registry_states_map_to_agent_states() {
    let temp = Temp::new("states");
    let (mut store, reader) = temp.open();
    fill(&mut store);
    let alpha = |reader: &Connection| {
        let rows = views::workstreams(reader).expect("workstreams");
        let id = by_name(&rows, ALPHA).id;
        views::live_graph(reader, id)
            .expect("query")
            .expect("graph")
            .agents[0]
            .state
    };
    for (status, state) in [
        ("idle", AgentState::Idle),
        ("waiting", AgentState::Idle),
        ("busy", AgentState::Working),
    ] {
        store
            .apply_registry(&[RegistryEvent::Changed {
                entry: parse_snapshot(
                    format!(
                        r#"[{{"pid":4101,"cwd":"/work/alpha","kind":"interactive",
                            "startedAt":1791000000000,"sessionId":"s-alpha","name":"alpha-1",
                            "status":"{status}"}}]"#
                    )
                    .as_bytes(),
                )
                .expect("entry")
                .remove(0),
                at_ms: 8000,
            }])
            .expect("changed");
        assert_eq!(alpha(&reader), state, "{status}");
    }
}

#[test]
fn any_links_orchestrator_reads_as_its_tab_and_other_ids_have_no_graph() {
    let temp = Temp::new("tab-ids");
    let (mut store, reader) = temp.open();
    fill(&mut store);
    let id = |sql: &str| -> i64 { reader.query_row(sql, [], |r| r.get(0)).expect(sql) };
    let s_alpha = id("SELECT id FROM agents WHERE session_id = 's-alpha' AND agent_id IS NULL");
    let graph = views::live_graph(&reader, s_alpha)
        .expect("query")
        .expect("a link's orchestrator");
    assert_eq!(graph.session_id, "s-alpha");
    let a2 = id("SELECT id FROM agents WHERE agent_id = 'a2'");
    assert_eq!(views::live_graph(&reader, a2).expect("query"), None);
    assert_eq!(views::live_graph(&reader, 9999).expect("query"), None);
}

/// The `workstreams-home` fixture, cold: its discovery graph, then each
/// transcript as one batch.
fn load_workstreams_home(store: &mut Store) {
    let root = fixture_home("workstreams-home");
    store
        .ingest_graph(&discover(&root).expect("discover"))
        .expect("graph");
    for file in transcripts(&root) {
        store
            .ingest(&Batch {
                path: file.as_path().into(),
                events: records(&file, 0..usize::MAX)
                    .into_iter()
                    .map(|(event, _)| event)
                    .collect(),
                checkpoint: None,
            })
            .expect("ingest");
    }
}

#[test]
fn a_tabs_header_with_its_latest_link_drawn_sums_the_whole_chain() {
    let temp = Temp::new("renamed");
    let (mut store, reader) = temp.open();
    load_workstreams_home(&mut store);
    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(rows.len(), 4, "{rows:?}");
    for tab in &rows {
        let graph = views::live_graph(&reader, tab.id)
            .expect("query")
            .expect("every fixture tab has a link");
        assert!(
            close(graph.session_cost_usd, tab.cost_usd.expect("priced")),
            "{}: {:?} drawn, {:?} total",
            tab.name,
            graph.session_cost_usd,
            tab.cost_usd
        );
    }
    // C1 -> C9 -> C5 was renamed twice: one tab still holds every link's $,
    // more than the drawn link's agents spent.
    let chain = by_name(&rows, "issue_demo-repo-83");
    let graph = views::live_graph(&reader, chain.id)
        .expect("query")
        .expect("a graph");
    assert_eq!(graph.session_id, "5e55c0c0-0000-4000-8000-0000000000c5");
    let drawn: f64 = graph.agents.iter().filter_map(|a| a.cost_usd).sum();
    assert!(drawn < chain.cost_usd.expect("priced"), "{drawn}");
}

#[test]
fn a_background_dispatch_runs_until_its_task_notification_not_its_launch_result() {
    let temp = Temp::new("background-dispatch");
    let (mut store, reader) = temp.open();
    // s-bg launched a9 in the background (its tool_result is the launch) and
    // a8 in the foreground (its tool_result is the answer).
    let launched = json!({"type": "user", "uuid": "g3", "timestamp": "2026-10-08T10:00:11.000Z",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_9", "content": "Launched."}]},
        "toolUseResult": {"status": "async_launched", "agentId": "a9"}});
    store
        .ingest(&batch(
            "s-bg.jsonl",
            &[
                prompt("g1", "2026-10-08T10:00:00.000Z"),
                reply(
                    "g2",
                    "2026-10-08T10:00:10.000Z",
                    10,
                    0,
                    &["toolu_9", "toolu_8"],
                ),
                launched,
                result("g4", "2026-10-08T10:00:40.000Z", "toolu_8"),
            ],
        ))
        .expect("s-bg");
    store
        .ingest_graph(&Graph {
            subagents: vec![
                dispatched("s-bg", "a9", "toolu_9"),
                dispatched("s-bg", "a8", "toolu_8"),
            ],
            ..Graph::default()
        })
        .expect("graph");
    store
        .apply_registry(&[appeared(
            r#"{"pid":4109,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
                "sessionId":"s-bg","name":"bg-9","status":"busy"}"#,
        )])
        .expect("live");
    let workers = |reader: &Connection| -> Vec<String> {
        let rows = views::workstreams(reader).expect("workstreams");
        let graph = views::live_graph(reader, rows[0].id)
            .expect("query")
            .expect("a graph");
        graph
            .agents
            .iter()
            .filter_map(|a| a.agent_id.clone())
            .collect()
    };
    assert_eq!(workers(&reader), ["a9"], "a8 answered; a9 only launched");

    let notice = json!({"type": "queue-operation", "timestamp": "2026-10-08T10:01:00.000Z",
        "content": "<task-notification>\n<task-id>a9</task-id>\n<status>completed</status>\n\
                    </task-notification>"});
    let mut more = batch("s-bg.jsonl", &[notice]);
    if let TailEvent::Record { offset, .. } = &mut more.events[0] {
        *offset = 900;
    }
    store.ingest(&more).expect("notified");
    assert_eq!(
        workers(&reader),
        Vec::<String>::new(),
        "its notification ends it"
    );
}
