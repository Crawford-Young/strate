//! Every lane (#108): a worker's finished and background state, and the
//! `launch` edge from a `claude --bg` command to the background session it
//! started, read through `views::agent_model`. Synthetic records, graphs
//! and registry rows go straight into a store in a temp dir; the
//! fixture-wide cases are in tests/model.rs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use strate_core::discovery::{AgentRef, Graph, Link, SubagentMeta, SubagentNode, discover};
use strate_core::registry::{Kind, RegistryEntry, RegistryEvent, Status};
use strate_core::store::Store;
use strate_core::store::views::{self, Lane, ModelAgent};
use strate_core::tail::{Batch, TailEvent};
use strate_core::time::parse_timestamp;

mod common;

use common::{Temp, batch as whole, fixture_home, records, transcripts};

// ---- builders -----------------------------------------------------------------

/// A store fed in-memory batches of transcripts under `<temp>/projects/-work-x`.
struct Home {
    t: Temp,
    store: Store,
    next: HashMap<PathBuf, u64>,
}

impl Home {
    fn new(name: &str) -> Self {
        let t = Temp::new(name);
        let store = Store::open_in(t.db(), "UTC").expect("open");
        Self {
            t,
            store,
            next: HashMap::new(),
        }
    }

    /// Where a transcript of `session` (or its subagent `agent`) sits.
    fn transcript(&self, session: &str, agent: Option<&str>) -> PathBuf {
        let project = self.t.path().join("projects/-work-x");
        match agent {
            None => project.join(format!("{session}.jsonl")),
            Some(a) => project
                .join(session)
                .join("subagents")
                .join(format!("agent-{a}.jsonl")),
        }
    }

    fn feed(&mut self, session: &str, agent: Option<&str>, lines: &[Value]) {
        let path: Arc<Path> = self.transcript(session, agent).into();
        let offset = self.next.entry(path.to_path_buf()).or_default();
        let mut events = Vec::new();
        for value in lines {
            events.push(TailEvent::Record {
                path: path.clone(),
                offset: *offset,
                value: value.clone(),
            });
            *offset += 100;
        }
        self.store
            .ingest(&Batch {
                path,
                events,
                checkpoint: None,
            })
            .expect("ingest");
    }

    /// Subagent `agent` of `session` dispatched by `parent` with
    /// `toolu_<agent>`, its meta `finished` as given.
    fn dispatched(
        &self,
        session: &str,
        agent: &str,
        parent: AgentRef,
        finished: Option<bool>,
    ) -> SubagentNode {
        let tool_use_id = format!("toolu_{agent}");
        SubagentNode {
            session_id: session.into(),
            agent_id: agent.into(),
            path: self.transcript(session, Some(agent)),
            meta: Some(SubagentMeta {
                tool_use_id: Some(tool_use_id.clone()),
                finished,
                ..SubagentMeta::default()
            }),
            link: Link::Dispatch {
                parent,
                tool_use_id,
            },
        }
    }

    fn graph(&mut self, subagents: Vec<SubagentNode>) {
        self.store
            .ingest_graph(&Graph {
                subagents,
                ..Graph::default()
            })
            .expect("graph");
    }

    fn model(&self) -> Vec<ModelAgent> {
        let reader = Store::open_reader(self.t.db()).expect("reader");
        views::agent_model(&reader).expect("model")
    }

    fn agent(&self, session: &str, agent: Option<&str>) -> ModelAgent {
        self.model()
            .into_iter()
            .find(|a| a.session_id == session && a.agent_id.as_deref() == agent)
            .unwrap_or_else(|| panic!("no agent {session} {agent:?}"))
    }

    fn id(&self, session: &str, agent: Option<&str>) -> i64 {
        self.agent(session, agent).id
    }

    /// The spawn parent of `session`'s orchestrator and how.
    fn via(&self, session: &str) -> (Option<i64>, Option<String>) {
        let a = self.agent(session, None);
        (a.parent, a.parent_via)
    }
}

fn orchestrator(session: &str) -> AgentRef {
    AgentRef::Session {
        session_id: session.into(),
    }
}

fn at(time: &str) -> String {
    format!("2026-10-20T{time}.000Z")
}

/// A user record at `time`.
fn prompt(time: &str) -> Value {
    json!({"type": "user", "timestamp": at(time), "cwd": "/work/x",
        "message": {"role": "user", "content": "Lorem."}})
}

/// A user record at `time` in a transcript of `root`'s `/clear` chain.
fn linked(root: &str, time: &str) -> Value {
    let mut v = prompt(time);
    v["session_id"] = json!(root);
    v
}

fn title(name: &str) -> Value {
    json!({"type": "custom-title", "customTitle": name})
}

/// An assistant record at `time` calling `tool` (`toolu_<id>`) on `command`.
fn shell(time: &str, id: &str, tool: &str, command: &str) -> Value {
    json!({"type": "assistant", "timestamp": at(time), "message": {"role": "assistant",
        "content": [{"type": "tool_use", "id": format!("toolu_{id}"), "name": tool,
            "input": {"command": command, "description": "Lorem"}}]}})
}

/// The tool_result of `toolu_<id>` at `time`, its text `text`.
fn result(time: &str, id: &str, text: &str) -> Value {
    json!({"type": "user", "timestamp": at(time), "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": format!("toolu_{id}"), "content": text}]},
        "toolUseResult": {"stdout": text, "stderr": "", "interrupted": false}})
}

/// The immediate result of background dispatch `toolu_<id>`.
fn async_launched(time: &str, id: &str) -> Value {
    json!({"type": "user", "timestamp": at(time), "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": format!("toolu_{id}"),
            "content": [{"type": "text", "text": "Async agent launched successfully."}]}]},
        "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": id}})
}

/// The task notification of subagent `agent`.
fn notified(time: &str, agent: &str) -> Value {
    json!({"type": "user", "timestamp": at(time), "message": {"role": "user", "content":
        format!("<task-notification>\n<task-id>{agent}</task-id>\n<status>completed</status>\n<summary>Lorem</summary>\n</task-notification>")}})
}

/// The SendMessage result of message `msg`, at `time`.
fn sent(time: &str, msg: &str) -> Value {
    json!({"type": "user", "timestamp": at(time), "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": format!("toolu_send_{msg}"), "content": "Lorem."}]},
        "toolUseResult": {"success": true, "msg_id": msg}})
}

/// Message `msg` received from another session at `time`.
fn peer(time: &str, msg: &str) -> Value {
    json!({"type": "user", "timestamp": at(time), "message": {"role": "user",
        "content": "Lorem ipsum."}, "origin": {"kind": "peer", "msg_id": msg, "name": "lorem"}})
}

/// A background registry row with short id `id` naming `session` as `name`.
fn background(id: &str, session: &str, name: Option<&str>) -> RegistryEvent {
    RegistryEvent::Appeared {
        entry: RegistryEntry {
            cwd: "/work/x".into(),
            kind: Kind::Background,
            started_at: 1,
            pid: None,
            id: Some(id.into()),
            session_id: Some(session.into()),
            name: name.map(str::to_string),
            status: None,
            waiting_for: None,
            state: None,
        },
        at_ms: 1_000,
    }
}

// ---- finished and background ----------------------------------------------------

#[test]
fn meta_finished_marks_a_worker_finished_and_sticks_across_later_scans() {
    let mut h = Home::new("lanes-meta-finished");
    h.feed("s1", None, &[prompt("10:00:00")]);
    let w1 = h.dispatched("s1", "w1", orchestrator("s1"), Some(true));
    let w2 = h.dispatched("s1", "w2", orchestrator("s1"), None);
    h.graph(vec![w1, w2]);
    assert!(h.agent("s1", Some("w1")).finished, "meta finished");
    assert!(!h.agent("s1", Some("w2")).finished, "still running");
    assert!(!h.agent("s1", None).finished, "an orchestrator never is");

    // A later scan whose meta lacks `finished` (or says false) keeps it.
    let w1 = h.dispatched("s1", "w1", orchestrator("s1"), None);
    let w2 = h.dispatched("s1", "w2", orchestrator("s1"), Some(false));
    h.graph(vec![w1, w2]);
    assert!(h.agent("s1", Some("w1")).finished, "sticky");
    assert!(!h.agent("s1", Some("w2")).finished);
}

#[test]
fn a_batch_on_a_linked_subagents_transcript_rereads_its_meta() {
    let mut h = Home::new("lanes-meta-reread");
    h.feed("s1", None, &[prompt("10:00:00")]);
    let w1 = h.dispatched("s1", "w1", orchestrator("s1"), None);
    let meta = w1.path.with_file_name("agent-w1.meta.json");
    std::fs::create_dir_all(meta.parent().expect("dir")).expect("dirs");
    std::fs::write(&meta, r#"{"agentType":"x","toolUseId":"toolu_w1"}"#).expect("meta");
    h.graph(vec![w1]);
    h.feed("s1", Some("w1"), &[prompt("10:00:10")]);
    assert!(!h.agent("s1", Some("w1")).finished);

    // The engine discovers a linked subagent only once: its next batch reads
    // the meta it finished in.
    std::fs::write(
        &meta,
        r#"{"agentType":"x","toolUseId":"toolu_w1","finished":true}"#,
    )
    .expect("meta");
    h.feed("s1", Some("w1"), &[prompt("10:00:20")]);
    assert!(h.agent("s1", Some("w1")).finished);
}

#[test]
fn a_dispatch_answer_or_task_notification_finishes_a_worker_and_async_marks_it_background() {
    let mut h = Home::new("lanes-dispatch-results");
    // w1's answer and w2's launch arrive before the graph, w3's after it.
    h.feed(
        "s1",
        None,
        &[
            prompt("10:00:00"),
            result("10:01:00", "w1", "Lorem."),
            async_launched("10:02:00", "w2"),
        ],
    );
    let nodes = ["w1", "w2", "w3", "w4"]
        .map(|w| h.dispatched("s1", w, orchestrator("s1"), None))
        .to_vec();
    h.graph(nodes);
    h.feed("s1", None, &[async_launched("10:03:00", "w3")]);

    let state = |h: &Home, w: &str| {
        let a = h.agent("s1", Some(w));
        (a.finished, a.background)
    };
    assert_eq!(state(&h, "w1"), (true, false), "answered");
    assert_eq!(state(&h, "w2"), (false, true), "launched in the background");
    assert_eq!(state(&h, "w3"), (false, true));
    assert_eq!(state(&h, "w4"), (false, false), "no result yet");

    h.feed("s1", None, &[notified("10:09:00", "w2")]);
    assert_eq!(state(&h, "w2"), (true, true), "its task notification");
    let s1 = h.agent("s1", None);
    assert_eq!((s1.finished, s1.background), (false, false));
}

#[test]
fn the_live_graph_draws_no_worker_whose_meta_says_finished() {
    let mut h = Home::new("lanes-live-finished");
    h.store
        .apply_registry(&[background("aaaa0001", "s1", None)])
        .expect("registry");
    h.feed("s1", None, &[prompt("10:00:00")]);
    let w1 = h.dispatched("s1", "w1", orchestrator("s1"), Some(true));
    let w2 = h.dispatched("s1", "w2", orchestrator("s1"), None);
    h.graph(vec![w1, w2]);
    let reader = Store::open_reader(h.t.db()).expect("reader");
    let graph = views::live_graph(&reader, h.id("s1", None))
        .expect("query")
        .expect("a graph");
    let drawn: Vec<Option<&str>> = graph.agents.iter().map(|a| a.agent_id.as_deref()).collect();
    assert_eq!(drawn, [None, Some("w2")]);
}

// ---- a worker whose lead exited (#115) -----------------------------------------------

/// `time` in unix ms.
fn ms(time: &str) -> i64 {
    parse_timestamp(&at(time)).expect("a timestamp")
}

/// The process (pid `pid`, started at `started`) of interactive session
/// `session`, as `claude agents` lists it.
fn process(session: &str, pid: u32, started: &str) -> RegistryEntry {
    RegistryEntry {
        cwd: "/work/x".into(),
        kind: Kind::Interactive,
        started_at: ms(started),
        pid: Some(pid),
        id: None,
        session_id: Some(session.into()),
        name: None,
        status: Some(Status::Busy),
        waiting_for: None,
        state: None,
    }
}

/// A poll at `time` that first listed `entry`.
fn appeared(entry: &RegistryEntry, time: &str) -> RegistryEvent {
    RegistryEvent::Appeared {
        entry: entry.clone(),
        at_ms: ms(time),
    }
}

/// A poll that no longer listed `entry`, last listed at `time`.
fn gone(entry: &RegistryEntry, time: &str) -> RegistryEvent {
    RegistryEvent::Gone {
        entry: entry.clone(),
        last_seen_ms: ms(time),
    }
}

/// Teammate `agent` of `session`, named `name`, of team-lorem.
fn teammate(h: &Home, session: &str, agent: &str, name: &str) -> SubagentNode {
    SubagentNode {
        session_id: session.into(),
        agent_id: agent.into(),
        path: h.transcript(session, Some(agent)),
        meta: Some(SubagentMeta {
            name: Some(name.into()),
            team_name: Some("team-lorem".into()),
            ..SubagentMeta::default()
        }),
        link: Link::Teammate {
            team_name: "team-lorem".into(),
            team_dir: None,
        },
    }
}

/// Teammate `name`'s idle notice in its lead's transcript, stamped `time`.
fn idle(time: &str, name: &str) -> Value {
    let body = json!({"type": "idle_notification", "from": name, "timestamp": at(time),
        "idleReason": "available"});
    json!({"type": "user", "timestamp": at(time), "message": {"role": "user", "content":
        format!("<teammate-message teammate_id=\"{name}\" color=\"blue\">\n{body}\n</teammate-message>")}})
}

impl Home {
    fn registry(&mut self, events: &[RegistryEvent]) {
        self.store.apply_registry(events).expect("registry");
    }

    fn finished(&self, session: &str, agent: &str) -> bool {
        self.agent(session, Some(agent)).finished
    }
}

#[test]
fn a_teammate_finishes_once_its_leads_process_is_gone_never_on_an_idle_notice() {
    let mut h = Home::new("lanes-teammate-ended");
    let lead = process("s1", 4101, "09:59:00");
    h.registry(&[appeared(&lead, "10:00:00")]);
    h.feed("s1", None, &[prompt("10:00:00")]);
    let mate = teammate(&h, "s1", "t1", "lorem-mate");
    h.graph(vec![mate]);
    h.feed("s1", Some("t1"), &[prompt("10:01:00")]);
    assert!(!h.finished("s1", "t1"), "its lead runs");

    // It rests: an idle teammate can be woken, so it has not finished.
    h.feed(
        "s1",
        None,
        &[idle("10:02:00", "lorem-mate"), prompt("10:03:00")],
    );
    h.registry(&[RegistryEvent::Changed {
        entry: lead.clone(),
        at_ms: ms("10:04:00"),
    }]);
    assert!(!h.finished("s1", "t1"), "an idle notice alone");

    h.registry(&[gone(&lead, "10:05:00")]);
    assert!(h.finished("s1", "t1"), "its lead's registry row is gone");
}

#[test]
fn a_worker_finishes_once_its_leads_process_is_gone() {
    let mut h = Home::new("lanes-worker-ended");
    let lead = process("s1", 4102, "09:59:00");
    h.registry(&[appeared(&lead, "10:00:00")]);
    h.feed(
        "s1",
        None,
        &[prompt("10:00:00"), async_launched("10:00:30", "w1")],
    );
    let nodes = ["w1", "w2"]
        .map(|w| h.dispatched("s1", w, orchestrator("s1"), None))
        .to_vec();
    h.graph(nodes);
    h.feed("s1", Some("w1"), &[prompt("10:01:00")]);
    h.feed("s1", Some("w2"), &[prompt("10:01:10")]);
    h.registry(&[RegistryEvent::Changed {
        entry: lead.clone(),
        at_ms: ms("10:02:00"),
    }]);
    let state = |h: &Home, w: &str| {
        let a = h.agent("s1", Some(w));
        (a.finished, a.background)
    };
    assert_eq!(state(&h, "w1"), (false, true), "a live lead's");
    assert_eq!(state(&h, "w2"), (false, false), "a live lead's");

    h.registry(&[gone(&lead, "10:02:00")]);
    assert_eq!(state(&h, "w1"), (true, true), "a background worker");
    assert_eq!(state(&h, "w2"), (true, false), "nor runs a foreground one");
    assert!(!h.agent("s1", None).finished, "an orchestrator never is");
}

#[test]
fn a_worker_of_a_session_never_listed_finishes_once_a_later_poll_omits_it() {
    let mut h = Home::new("lanes-worker-unlisted");
    h.feed("s1", None, &[prompt("10:00:00")]);
    let mate = teammate(&h, "s1", "t1", "lorem-mate");
    h.graph(vec![mate]);
    h.feed("s1", Some("t1"), &[prompt("10:01:00")]);
    let other = process("s9", 4109, "09:00:00");
    h.registry(&[appeared(&other, "10:00:30")]);
    assert!(
        !h.finished("s1", "t1"),
        "no poll since its last record: maybe not listed yet"
    );
    h.registry(&[RegistryEvent::Changed {
        entry: other,
        at_ms: ms("10:01:00"),
    }]);
    assert!(
        h.finished("s1", "t1"),
        "a poll at its last record omitted s1"
    );

    // No record: nothing says when it last ran.
    h.graph(vec![teammate(&h, "s1", "t2", "ipsum-mate")]);
    assert!(!h.finished("s1", "t2"));
}

#[test]
fn a_new_process_of_the_lead_neither_revives_a_finished_worker_nor_finishes_its_own() {
    let mut h = Home::new("lanes-worker-resumed");
    let first = process("s1", 4103, "09:59:00");
    h.registry(&[appeared(&first, "10:00:00")]);
    h.feed("s1", None, &[prompt("10:00:00")]);
    h.graph(vec![
        teammate(&h, "s1", "t1", "lorem-mate"),
        teammate(&h, "s1", "t2", "ipsum-mate"),
    ]);
    h.feed("s1", Some("t1"), &[prompt("10:01:00")]);
    h.registry(&[gone(&first, "10:02:00")]);
    assert!(h.finished("s1", "t1"));

    // The session resumed in a new process, which spawned t2.
    let second = process("s1", 4104, "10:30:00");
    h.registry(&[appeared(&second, "10:30:01")]);
    h.feed("s1", Some("t2"), &[prompt("10:31:00")]);
    h.registry(&[RegistryEvent::Changed {
        entry: second,
        at_ms: ms("10:32:00"),
    }]);
    assert!(h.finished("s1", "t1"), "finished stays finished");
    assert!(!h.finished("s1", "t2"), "the live process runs it");
}

#[test]
fn the_live_graph_draws_no_worker_of_its_leads_earlier_process() {
    let mut h = Home::new("lanes-live-resumed");
    let first = process("s1", 4106, "09:59:00");
    h.registry(&[appeared(&first, "10:00:00")]);
    h.feed("s1", None, &[prompt("10:00:00")]);
    h.graph(vec![
        teammate(&h, "s1", "t1", "lorem-mate"),
        teammate(&h, "s1", "t2", "ipsum-mate"),
    ]);
    h.feed("s1", Some("t1"), &[prompt("10:01:00")]);
    h.registry(&[gone(&first, "10:02:00")]);
    let second = process("s1", 4107, "10:30:00");
    h.registry(&[appeared(&second, "10:30:01")]);
    h.feed("s1", None, &[prompt("10:30:30")]);
    h.feed("s1", Some("t2"), &[prompt("10:31:00")]);
    let reader = Store::open_reader(h.t.db()).expect("reader");
    let graph = views::live_graph(&reader, h.id("s1", None))
        .expect("query")
        .expect("a graph");
    assert!(graph.live);
    let drawn: Vec<Option<&str>> = graph.agents.iter().map(|a| a.agent_id.as_deref()).collect();
    assert_eq!(drawn, [None, Some("t2")], "t1 ended with the first process");
}

#[test]
fn a_worker_of_a_clear_chain_runs_while_any_link_is_listed() {
    // s1 /cleared into s2: one process, whose registry row names s1.
    let mut h = Home::new("lanes-worker-chain");
    let lead = process("s1", 4105, "09:59:00");
    h.registry(&[appeared(&lead, "10:00:00")]);
    h.feed("s1", None, &[prompt("10:00:00")]);
    h.feed("s2", None, &[linked("s1", "10:20:00")]);
    h.graph(vec![h.dispatched("s2", "w1", orchestrator("s2"), None)]);
    h.feed("s2", Some("w1"), &[prompt("10:21:00")]);
    h.registry(&[RegistryEvent::Changed {
        entry: lead.clone(),
        at_ms: ms("10:22:00"),
    }]);
    assert!(!h.finished("s2", "w1"), "its chain's process is listed");
    h.registry(&[gone(&lead, "10:22:00")]);
    assert!(h.finished("s2", "w1"));
}

// ---- a worker's edge before discovery ----------------------------------------------

/// Writes subagent `agent`'s meta as Claude Code does at spawn, before the
/// transcript's first record.
fn spawn_meta(h: &Home, session: &str, agent: &str, meta: &str) {
    let path = h
        .transcript(session, Some(agent))
        .with_file_name(format!("agent-{agent}.meta.json"));
    std::fs::create_dir_all(path.parent().expect("dir")).expect("dirs");
    std::fs::write(path, meta).expect("meta");
}

/// The live graph's edges of tab `session`, as (kind, parent, child) agent ids.
fn drawn_edges(h: &Home, session: &str) -> Vec<(String, Option<String>, Option<String>)> {
    let reader = Store::open_reader(h.t.db()).expect("reader");
    let graph = views::live_graph(&reader, h.id(session, None))
        .expect("query")
        .expect("a graph");
    let key = |id: i64| {
        graph
            .agents
            .iter()
            .find(|a| a.id == id)
            .expect("a drawn agent")
            .agent_id
            .clone()
    };
    let mut edges: Vec<_> = graph
        .edges
        .iter()
        .map(|e| (e.kind.clone(), key(e.parent), key(e.child)))
        .collect();
    edges.sort();
    edges
}

#[test]
fn a_nested_background_dispatch_hangs_under_its_dispatcher_before_discovery_links_it() {
    // The live sequence (#109 QA): code-20's subagent qa dispatches a
    // background haiku. The discovery pass that links it re-read every
    // transcript until #121, which took ~20 s on a real home, so the child's
    // records land and it is drawn before any pass stores its dispatch edge.
    let mut h = Home::new("lanes-nested-unlinked");
    h.store
        .apply_registry(&[background("aaaa0001", "s1", None)])
        .expect("registry");
    h.feed(
        "s1",
        None,
        &[
            prompt("10:00:00"),
            shell("10:00:01", "qa", "Agent", "Lorem"),
        ],
    );
    let qa = h.dispatched("s1", "qa", orchestrator("s1"), None);
    h.graph(vec![qa]);
    h.feed(
        "s1",
        Some("qa"),
        &[
            prompt("10:00:02"),
            shell("10:00:03", "hk", "Agent", "Lorem"),
        ],
    );
    spawn_meta(
        &h,
        "s1",
        "hk",
        r#"{"agentType":"claude","toolUseId":"toolu_hk","parentAgentId":"qa","spawnDepth":2,
            "requestShape":"background"}"#,
    );
    h.feed("s1", Some("hk"), &[prompt("10:00:04")]);
    h.feed("s1", Some("qa"), &[async_launched("10:00:04", "hk")]);
    // And one the orchestrator dispatched: its meta names no parent agent.
    h.feed("s1", None, &[shell("10:00:05", "w2", "Agent", "Lorem")]);
    spawn_meta(
        &h,
        "s1",
        "w2",
        r#"{"agentType":"x","toolUseId":"toolu_w2","spawnDepth":1}"#,
    );
    h.feed("s1", Some("w2"), &[prompt("10:00:06")]);

    let edge = |kind: &str, parent: Option<&str>, child: &str| {
        (
            kind.to_string(),
            parent.map(str::to_string),
            Some(child.to_string()),
        )
    };
    assert_eq!(
        drawn_edges(&h, "s1"),
        [
            edge("dispatch", None, "qa"),
            edge("dispatch", None, "w2"),
            edge("dispatch", Some("qa"), "hk"),
        ],
        "each under its dispatcher, from its spawn meta"
    );
    assert!(
        h.agent("s1", Some("hk")).background,
        "its launch result reads through the edge"
    );
}

#[test]
fn a_worker_whose_tool_use_is_not_stored_yet_hangs_under_the_parent_its_meta_names() {
    let mut h = Home::new("lanes-meta-parent");
    h.store
        .apply_registry(&[background("aaaa0001", "s1", None)])
        .expect("registry");
    h.feed("s1", None, &[prompt("10:00:00")]);
    let qa = h.dispatched("s1", "qa", orchestrator("s1"), None);
    h.graph(vec![qa]);
    h.feed("s1", Some("qa"), &[prompt("10:00:02")]);
    // The child's batch beat its dispatcher's tool_use record; one naming
    // itself, or an agent not stored, names no parent.
    spawn_meta(
        &h,
        "s1",
        "hk",
        r#"{"toolUseId":"toolu_hk","parentAgentId":"qa","spawnDepth":2}"#,
    );
    spawn_meta(&h, "s1", "me", r#"{"parentAgentId":"me"}"#);
    spawn_meta(&h, "s1", "lo", r#"{"parentAgentId":"lorem"}"#);
    for w in ["hk", "me", "lo"] {
        h.feed("s1", Some(w), &[prompt("10:00:04")]);
    }
    let edge = |parent: Option<&str>, child: &str| {
        (
            "dispatch".to_string(),
            parent.map(str::to_string),
            Some(child.to_string()),
        )
    };
    assert_eq!(
        drawn_edges(&h, "s1"),
        [
            edge(None, "lo"),
            edge(None, "me"),
            edge(None, "qa"),
            edge(Some("qa"), "hk"),
        ]
    );
}

#[test]
fn a_worker_with_no_meta_yet_hangs_under_its_sessions_orchestrator() {
    let mut h = Home::new("lanes-no-meta");
    h.store
        .apply_registry(&[background("aaaa0001", "s1", None)])
        .expect("registry");
    h.feed("s1", None, &[prompt("10:00:00")]);
    let qa = h.dispatched("s1", "qa", orchestrator("s1"), None);
    h.graph(vec![qa]);
    h.feed("s1", Some("qa"), &[prompt("10:00:02")]);
    // No meta, so nothing names its dispatcher: at worst it hangs under its
    // nearest drawn ancestor, never edgeless (#109).
    h.feed("s1", Some("w9"), &[prompt("10:00:04")]);
    assert_eq!(
        drawn_edges(&h, "s1"),
        [
            ("dispatch".to_string(), None, Some("qa".to_string())),
            ("dispatch".to_string(), None, Some("w9".to_string())),
        ]
    );
}

// ---- launch edges -----------------------------------------------------------------

#[test]
fn a_launch_links_the_session_its_resume_id_names() {
    let mut h = Home::new("lanes-launch-resume");
    h.feed("s2", None, &[prompt("09:00:00")]);
    h.feed(
        "s1",
        None,
        &[
            prompt("10:00:00"),
            shell("10:01:00", "l1", "Bash", "claude --bg --resume s2 -n lorem"),
            result("10:01:01", "l1", "Started abcd1234."),
        ],
    );
    assert_eq!(h.via("s2"), (Some(h.id("s1", None)), Some("launch".into())));
}

#[test]
fn a_launch_links_the_background_row_whose_short_id_its_result_prints() {
    let mut h = Home::new("lanes-launch-id");
    h.store
        .apply_registry(&[
            background("abcd1234", "s3", None),
            background("abcd123", "s4", None),
        ])
        .expect("registry");
    h.feed("s3", None, &[prompt("10:01:05")]);
    h.feed("s4", None, &[prompt("10:01:05")]);
    // PowerShell, a subagent's transcript, and the result in a later batch.
    h.feed(
        "s1",
        Some("w1"),
        &[shell(
            "10:01:00",
            "l1",
            "PowerShell",
            r#"& "C:\Users\x\.local\bin\claude.exe" --bg -n other "Lorem.""#,
        )],
    );
    assert_eq!(h.via("s3"), (None, None), "no result yet");
    h.feed(
        "s1",
        Some("w1"),
        &[result(
            "10:01:01",
            "l1",
            "Started background session abcd1234.\n",
        )],
    );
    assert_eq!(
        h.via("s3"),
        (Some(h.id("s1", Some("w1"))), Some("launch".into()))
    );
    assert_eq!(h.via("s4"), (None, None), "a whole token only");
}

#[test]
fn a_launch_links_the_earliest_background_session_named_as_it_since_it_ran() {
    let mut h = Home::new("lanes-launch-name");
    h.feed(
        "s1",
        None,
        &[
            prompt("10:00:00"),
            shell("10:01:00", "l1", "Bash", "claude --bg -n ' Lorem-Unit '"),
            result("10:01:01", "l1", "Lorem."),
            shell("10:01:02", "l2", "Bash", "claude --bg -n lorem-unit"),
            result("10:01:03", "l2", "Lorem."),
        ],
    );
    // s5 started before both launches; s7 (by its own title) and s6 (by
    // its registry name) after, s7 first. s8 has no transcript, so no
    // start. Each launch takes the earliest session not taken yet.
    h.store
        .apply_registry(&[
            background("aaaa0001", "s5", Some("lorem-unit")),
            background("aaaa0002", "s6", Some("LOREM-UNIT")),
            background("aaaa0003", "s7", None),
            background("aaaa0004", "s8", Some("lorem-unit")),
        ])
        .expect("registry");
    h.feed("s5", None, &[prompt("09:59:00")]);
    h.feed("s6", None, &[prompt("10:01:05")]);
    h.feed("s7", None, &[title("lorem-unit"), prompt("10:01:04")]);
    let s1 = Some(h.id("s1", None));
    let launch = Some("launch".to_string());
    assert_eq!(h.via("s7"), (s1, launch.clone()), "l1, by its title");
    assert_eq!(h.via("s6"), (s1, launch), "l2, by its registry name");
    assert_eq!(h.via("s5"), (None, None), "started before the launches");
}

#[test]
fn a_resume_beats_a_printed_id_which_beats_a_name() {
    let mut h = Home::new("lanes-launch-precedence");
    h.store
        .apply_registry(&[
            background("abcd1234", "s3", Some("lorem")),
            background("abcd5678", "s4", Some("lorem")),
        ])
        .expect("registry");
    for s in ["s2", "s3", "s4"] {
        h.feed(s, None, &[prompt("10:02:00")]);
    }
    h.feed(
        "s1",
        None,
        &[
            prompt("10:00:00"),
            shell("10:01:00", "l1", "Bash", "claude --bg -r s2 -n lorem"),
            result("10:01:01", "l1", "abcd1234"),
            shell("10:01:10", "l2", "Bash", "claude --bg -n lorem"),
            result("10:01:11", "l2", "abcd5678"),
        ],
    );
    let s1 = Some(h.id("s1", None));
    let launch = Some("launch".to_string());
    assert_eq!(h.via("s2"), (s1, launch.clone()), "resume");
    assert_eq!(h.via("s4"), (s1, launch), "printed id");
    assert_eq!(h.via("s3"), (None, None), "its name alone loses to l2's id");
}

#[test]
fn a_launch_beats_a_message_and_a_child_takes_its_earliest_launch() {
    let mut h = Home::new("lanes-launch-message");
    // s6 messages s2 first; s1 and later s9 both launch it.
    h.feed("s6", None, &[sent("10:00:30", "m1")]);
    h.feed("s2", None, &[peer("10:00:31", "m1"), prompt("10:02:00")]);
    h.feed(
        "s9",
        None,
        &[
            shell("10:01:30", "l9", "Bash", "claude --bg --resume s2"),
            result("10:01:31", "l9", "Lorem."),
        ],
    );
    h.feed(
        "s1",
        None,
        &[
            shell("10:01:00", "l1", "Bash", "claude --bg --resume s2"),
            result("10:01:01", "l1", "Lorem."),
        ],
    );
    assert_eq!(h.via("s2"), (Some(h.id("s1", None)), Some("launch".into())));
}

#[test]
fn a_launch_closing_a_spawn_cycle_is_refused() {
    let mut h = Home::new("lanes-launch-cycle");
    // s1's subagent w1 relaunches s1 in the background: s1 -> w1 -> s1.
    h.feed("s1", None, &[prompt("10:00:00")]);
    h.feed(
        "s1",
        Some("w1"),
        &[
            shell("10:01:00", "l1", "Bash", "claude --bg --resume s1"),
            result("10:01:01", "l1", "Lorem."),
        ],
    );
    let w1 = h.dispatched("s1", "w1", orchestrator("s1"), None);
    h.graph(vec![w1]);
    assert_eq!(h.via("s1"), (None, None));
    let w1 = h.agent("s1", Some("w1"));
    assert_eq!(w1.parent_via.as_deref(), Some("dispatch"));
}

#[test]
fn a_printed_short_id_links_the_first_link_of_its_chain_since_the_launch() {
    let mut h = Home::new("lanes-launch-id-clear");
    h.feed(
        "s1",
        None,
        &[
            shell("10:01:00", "l1", "Bash", "claude --bg -n lorem"),
            result("10:01:01", "l1", "Started abcd1234."),
        ],
    );
    // The launch started s3, which then /cleared into s3b: the registry
    // row, keyed by its short id, now names s3b.
    h.store
        .apply_registry(&[background("abcd1234", "s3", None)])
        .expect("registry");
    h.feed("s3", None, &[linked("s3", "10:01:05")]);
    h.feed("s3b", None, &[linked("s3", "10:30:00")]);
    h.store
        .apply_registry(&[background("abcd1234", "s3b", None)])
        .expect("registry");
    assert_eq!(h.via("s3"), (Some(h.id("s1", None)), Some("launch".into())));
    assert_eq!(h.via("s3b"), (None, None), "a later link of the chain");
}

#[test]
fn a_launch_naming_its_child_by_id_beats_an_earlier_launch_by_name_alone() {
    let mut h = Home::new("lanes-launch-two-pass");
    h.store
        .apply_registry(&[
            background("abcd1234", "s3", Some("lorem")),
            background("abcd5678", "s4", Some("lorem")),
        ])
        .expect("registry");
    h.feed("s3", None, &[prompt("10:01:20")]);
    h.feed("s4", None, &[prompt("10:01:30")]);
    // s1's launch by name ran first; s9's later one printed s3's short id.
    h.feed(
        "s1",
        None,
        &[
            shell("10:01:00", "l1", "Bash", "claude --bg -n lorem"),
            result("10:01:01", "l1", "Lorem."),
        ],
    );
    h.feed(
        "s9",
        None,
        &[
            shell("10:01:10", "l9", "Bash", "claude --bg -n other"),
            result("10:01:11", "l9", "Started abcd1234."),
        ],
    );
    let launch = Some("launch".to_string());
    assert_eq!(h.via("s3"), (Some(h.id("s9", None)), launch.clone()));
    assert_eq!(
        h.via("s4"),
        (Some(h.id("s1", None)), launch),
        "l1 takes the next lorem still free"
    );
}

#[test]
fn launches_go_in_time_order_however_their_timestamps_are_spelled() {
    let mut h = Home::new("lanes-launch-time-order");
    h.store
        .apply_registry(&[background("aaaa0001", "s3", Some("lorem"))])
        .expect("registry");
    h.feed("s3", None, &[prompt("10:01:01")]);
    // As text, s1's earlier "…10:01:00Z" sorts after s9's "…10:01:00.500Z".
    let mut l1 = shell("10:01:00", "l1", "Bash", "claude --bg -n lorem");
    l1["timestamp"] = json!("2026-10-20T10:01:00Z");
    let mut l9 = shell("10:01:00", "l9", "Bash", "claude --bg -n lorem");
    l9["timestamp"] = json!("2026-10-20T10:01:00.500Z");
    h.feed("s9", None, &[l9]);
    h.feed("s1", None, &[l1]);
    assert_eq!(h.via("s3"), (Some(h.id("s1", None)), Some("launch".into())));
}

#[test]
fn a_batch_rebuilds_the_launch_edges_only_when_it_changes_what_they_read() {
    let mut h = Home::new("lanes-launch-inputs");
    h.store
        .apply_registry(&[background("aaaa0001", "s3", Some("lorem"))])
        .expect("registry");
    h.feed(
        "s1",
        None,
        &[shell("10:01:00", "l1", "Bash", "claude --bg -n lorem")],
    );
    h.feed("s3", None, &[prompt("10:01:05")]);
    let launched = (Some(h.id("s1", None)), Some("launch".to_string()));
    assert_eq!(h.via("s3"), launched);

    // The edge is wiped behind the store's back, so a batch that rebuilds
    // the launch edges shows by restoring it. Rebuilding on every
    // orchestrator batch cost a full pass over every launch (#108).
    let raw = rusqlite::Connection::open(h.t.db()).expect("raw");
    let wipe = || {
        raw.execute("DELETE FROM edges WHERE kind = 'launch'", [])
            .expect("wipe")
    };
    wipe();
    h.feed("s3", None, &[prompt("10:05:00")]);
    h.feed("s1", None, &[prompt("10:06:00")]);
    assert_eq!(h.via("s3"), (None, None), "no start, name or chain moved");
    h.feed("s3", None, &[title("Lorem")]);
    assert_eq!(h.via("s3"), launched, "a new name");
    wipe();
    h.feed("s3", None, &[prompt("10:01:04")]);
    assert_eq!(h.via("s3"), launched, "an earlier start");
    wipe();
    h.feed("s3", None, &[linked("s0", "10:07:00")]);
    assert_eq!(h.via("s3"), launched, "joined a chain");
}

// ---- lanes-home: cold and incremental -----------------------------------------------

const L1: &str = "5e55f2f2-0000-4000-8000-0000000000e1";
const L2: &str = "5e55f2f2-0000-4000-8000-0000000000e2";
const L3: &str = "5e55f2f2-0000-4000-8000-0000000000e3";
const L4: &str = "5e55f2f2-0000-4000-8000-0000000000e4";
const L5: &str = "5e55f2f2-0000-4000-8000-0000000000e5";

fn home() -> PathBuf {
    fixture_home("lanes-home")
}

/// The registry rows the generator describes: L3, started by L2's launch;
/// L1's process, live; L5's, gone after 10:53.
fn registry() -> Vec<RegistryEvent> {
    let l5 = process(L5, 4205, "10:49:00");
    vec![
        background("c0ffee01", L3, Some("lorem-unit")),
        appeared(&process(L1, 4201, "09:59:00"), "10:00:00"),
        appeared(&l5, "10:50:00"),
        gone(&l5, "10:53:00"),
    ]
}

fn cold(t: &Temp) -> Vec<ModelAgent> {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&home()).expect("discover"))
        .expect("graph");
    for path in transcripts(&home()) {
        store.ingest(&whole(&path)).expect("ingest");
    }
    store.apply_registry(&registry()).expect("registry");
    drop(store);
    views::agent_model(&Store::open_reader(t.db()).expect("reader")).expect("model")
}

/// Each agent keyed by (session, agent id): its lane, parent (by key) and
/// how, finished and background.
type Keyed = (
    (String, Option<String>),
    Lane,
    Option<(String, Option<String>)>,
    Option<String>,
    bool,
    bool,
);

fn keyed(model: &[ModelAgent]) -> Vec<Keyed> {
    let keys: HashMap<i64, (String, Option<String>)> = model
        .iter()
        .map(|a| (a.id, (a.session_id.clone(), a.agent_id.clone())))
        .collect();
    let mut out: Vec<Keyed> = model
        .iter()
        .map(|a| {
            (
                keys[&a.id].clone(),
                a.lane,
                a.parent.map(|p| keys[&p].clone()),
                a.parent_via.clone(),
                a.finished,
                a.background,
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn lanes_home_reads_as_its_generator_describes() {
    let t = Temp::new("lanes-home-cold");
    let model = keyed(&cold(&t));
    let key = |s: &str, a: Option<&str>| (s.to_string(), a.map(str::to_string));
    let row = |s: &str, a: Option<&str>| {
        model
            .iter()
            .find(|k| k.0 == key(s, a))
            .unwrap_or_else(|| panic!("{s} {a:?}"))
            .clone()
    };
    let l1 = Some(key(L1, None));
    let dispatch = Some("dispatch".to_string());
    let worker = |a: &str, finished: bool, background: bool| {
        (
            key(L1, Some(a)),
            Lane::Subagent,
            l1.clone(),
            dispatch.clone(),
            finished,
            background,
        )
    };
    assert_eq!(
        row(L1, Some("a9100000000000001")),
        worker("a9100000000000001", true, false),
        "answered"
    );
    assert_eq!(
        row(L1, Some("a9100000000000002")),
        worker("a9100000000000002", true, true),
        "meta finished"
    );
    assert_eq!(
        row(L1, Some("a9100000000000003")),
        worker("a9100000000000003", false, true),
        "still running"
    );
    assert_eq!(
        row(L1, Some("a9100000000000004")),
        worker("a9100000000000004", true, true),
        "its task notification"
    );
    let l5 = Some(key(L5, None));
    assert_eq!(
        row(L5, Some("a9100000000000005")),
        (
            key(L5, Some("a9100000000000005")),
            Lane::Subagent,
            l5.clone(),
            dispatch.clone(),
            true,
            true
        ),
        "a background worker whose lead exited"
    );
    assert_eq!(
        row(L5, Some("a9100000000000006")),
        (
            key(L5, Some("a9100000000000006")),
            Lane::Subagent,
            l5,
            Some("teammate".into()),
            true,
            false
        ),
        "a teammate whose lead exited"
    );
    assert_eq!(
        row(L3, None),
        (
            key(L3, None),
            Lane::BackgroundSession,
            Some(key(L2, None)),
            Some("launch".into()),
            false,
            false
        ),
        "the launch beats L4's message"
    );
    for s in [L1, L2, L4, L5] {
        assert_eq!(row(s, None).2, None, "{s}");
    }
}

/// `path`'s lines `range` as one batch.
fn part(path: &Path, range: std::ops::Range<usize>) -> Option<Batch> {
    let events: Vec<TailEvent> = records(path, range).into_iter().map(|(e, _)| e).collect();
    (!events.is_empty()).then(|| Batch {
        path: path.into(),
        events,
        checkpoint: None,
    })
}

#[test]
fn lanes_home_built_in_any_order_equals_a_cold_build() {
    let ct = Temp::new("lanes-cold");
    let want = keyed(&cold(&ct));

    // The registry first, then every transcript's second half in reverse
    // order, the graph, and the first halves in order.
    let t = Temp::new("lanes-incremental");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store.apply_registry(&registry()).expect("registry");
    let paths = transcripts(&home());
    let halves: Vec<usize> = paths
        .iter()
        .map(|p| records(p, 0..usize::MAX).len().div_ceil(2))
        .collect();
    for (path, half) in paths.iter().zip(&halves).rev() {
        if let Some(b) = part(path, *half..usize::MAX) {
            store.ingest(&b).expect("second half");
        }
    }
    store
        .ingest_graph(&discover(&home()).expect("discover"))
        .expect("graph");
    for (path, half) in paths.iter().zip(&halves) {
        if let Some(b) = part(path, 0..*half) {
            store.ingest(&b).expect("first half");
        }
    }
    let reader = Store::open_reader(t.db()).expect("reader");
    assert_eq!(keyed(&views::agent_model(&reader).expect("model")), want);

    // A rebuild from the stored rows changes nothing.
    store.rebuild_model().expect("rebuild");
    assert_eq!(keyed(&views::agent_model(&reader).expect("model")), want);
}
