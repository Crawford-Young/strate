//! The generic agent model (#107): each agent's lane, role, own charge
//! (its branch's PR's closing issue, else its name), effective charge
//! (else its nearest spawn ancestor's, else Unlinked) and spawn parent
//! (dispatch, teammate or first messager), read through
//! `views::agent_model`. Focused cases feed synthetic records, graphs,
//! GitHub repos and registry rows straight into a store; the last ones run
//! over `tests/fixtures/model-home`, cold and incremental. Every db lives
//! in a temp dir.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use strate_core::charge::{Charge, ChargeKind};
use strate_core::discovery::{AgentRef, Graph, Link, SubagentMeta, SubagentNode, discover};
use strate_core::github::{Issue, Pr, Repo, Runner, fetch_repo};
use strate_core::registry::{Kind, RegistryEntry, RegistryEvent};
use strate_core::store::Store;
use strate_core::store::views::{self, ChargeSource, Lane, ModelAgent};
use strate_core::tail::{Batch, TailEvent};

mod common;

use common::{Temp, batch as whole, fixture_home, fixtures, records, transcripts};

// ---- builders -----------------------------------------------------------------

fn issue(repo: &str, number: u64) -> Charge {
    Charge {
        kind: ChargeKind::Issue,
        repo: Some(repo.into()),
        number: Some(number),
        topic: None,
    }
}

fn charge(
    kind: ChargeKind,
    repo: Option<&str>,
    number: Option<u64>,
    topic: Option<&str>,
) -> Charge {
    Charge {
        kind,
        repo: repo.map(str::to_string),
        number,
        topic: topic.map(str::to_string),
    }
}

/// Where a transcript of `session` (or its subagent `agent`) sits.
fn transcript(session: &str, agent: Option<&str>) -> PathBuf {
    let project = Path::new("/cfg/projects/-work-x");
    match agent {
        None => project.join(format!("{session}.jsonl")),
        Some(a) => project
            .join(session)
            .join("subagents")
            .join(format!("agent-{a}.jsonl")),
    }
}

/// A user record at 2026-10-20 `at` in `cwd` on `branch`.
fn record(at: &str, cwd: &str, branch: &str) -> Value {
    json!({"type": "user", "timestamp": format!("2026-10-20T{at}.000Z"), "cwd": cwd,
        "gitBranch": branch, "message": {"role": "user", "content": "Lorem."}})
}

/// `record` written by the process whose first session is `root`.
fn rooted(at: &str, root: &str) -> Value {
    let mut r = record(at, "/work/x", "main");
    r["session_id"] = json!(root);
    r
}

fn title(name: &str) -> Value {
    json!({"type": "custom-title", "customTitle": name})
}

/// The SendMessage result of message `msg`, at `at`.
fn sent(at: &str, msg: &str) -> Value {
    json!({"type": "user", "timestamp": format!("2026-10-20T{at}.000Z"),
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_send", "content": "Lorem."}]},
        "toolUseResult": {"success": true, "msg_id": msg}})
}

/// Message `msg` received from another session at `at`.
fn peer(at: &str, msg: &str) -> Value {
    json!({"type": "user", "timestamp": format!("2026-10-20T{at}.000Z"),
        "message": {"role": "user", "content": "Lorem ipsum."},
        "origin": {"kind": "peer", "msg_id": msg, "name": "lorem"}})
}

fn orchestrator(session: &str) -> AgentRef {
    AgentRef::Session {
        session_id: session.into(),
    }
}

fn subagent(session: &str, agent: &str) -> AgentRef {
    AgentRef::Subagent {
        session_id: session.into(),
        agent_id: agent.into(),
    }
}

/// Subagent `agent` of `session`, dispatched by `parent`, with meta `name`
/// and `description`.
fn dispatched(
    session: &str,
    agent: &str,
    parent: AgentRef,
    name: Option<&str>,
    description: Option<&str>,
) -> SubagentNode {
    let tool_use_id = format!("toolu_{agent}");
    SubagentNode {
        session_id: session.into(),
        agent_id: agent.into(),
        path: transcript(session, Some(agent)),
        meta: Some(SubagentMeta {
            name: name.map(str::to_string),
            description: description.map(str::to_string),
            tool_use_id: Some(tool_use_id.clone()),
            ..SubagentMeta::default()
        }),
        link: Link::Dispatch {
            parent,
            tool_use_id,
        },
    }
}

fn graph(subagents: Vec<SubagentNode>) -> Graph {
    Graph {
        subagents,
        ..Graph::default()
    }
}

/// PR `number` closing `issue`, opened `created` on branch `head`.
fn pr(issue: i64, number: i64, created: &str, head: &str) -> Pr {
    Pr {
        number,
        issue,
        state: "OPEN".into(),
        created_at: created.into(),
        merged_at: None,
        url: format!("https://github.com/example/x/pull/{number}"),
        head_ref: Some(head.into()),
    }
}

/// A repo of open issues, each closed by its `prs`.
fn repo(prs: Vec<Pr>) -> Repo {
    let mut issues: Vec<Issue> = Vec::new();
    for p in prs {
        match issues.iter_mut().find(|i| i.number == p.issue) {
            Some(i) => i.prs.push(p),
            None => issues.push(Issue {
                number: p.issue,
                title: "Lorem".into(),
                state: "OPEN".into(),
                state_reason: None,
                created_at: "2026-10-01T00:00:00Z".into(),
                closed_at: None,
                parent: None,
                sub_total: 0,
                sub_completed: 0,
                reopened: None,
                prs: vec![p],
            }),
        }
    }
    Repo {
        issues,
        reverts: vec![],
    }
}

/// A registry row of `kind` naming `session`, listed as `name` (keyed by
/// its background id, else by the session).
fn listed(kind: Kind, session: &str, name: Option<&str>) -> RegistryEvent {
    let background = kind == Kind::Background;
    RegistryEvent::Appeared {
        entry: RegistryEntry {
            cwd: "/work/x".into(),
            kind,
            started_at: 1,
            pid: None,
            id: background.then(|| format!("bg-{session}")),
            session_id: Some(session.into()),
            name: name.map(str::to_string),
            status: None,
            waiting_for: None,
            state: None,
        },
        at_ms: 1_000,
    }
}

/// A store fed in-memory batches, each line at its own offset.
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

    fn feed(&mut self, session: &str, agent: Option<&str>, lines: &[Value]) {
        let path: Arc<Path> = transcript(session, agent).into();
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
}

/// An agent's effective charge and where it came from.
fn charged(a: &ModelAgent) -> (Option<Charge>, ChargeSource) {
    (a.charge.clone(), a.charge_source)
}

// ---- own charge -----------------------------------------------------------------

#[test]
fn the_own_charge_is_the_branchs_pr_closing_issue_before_the_name() {
    let mut h = Home::new("model-branch");
    h.feed(
        "s1",
        None,
        &[
            title("impl_issue_strate-9"),
            record("10:00:00", "/work/strate", "feat/x"),
        ],
    );
    let a = h.agent("s1", None);
    assert_eq!(charged(&a), (Some(issue("strate", 9)), ChargeSource::Name));
    assert_eq!(a.role.as_deref(), Some("impl"));

    // PR 50 heads feat/x and closes #6 and #5: the lowest.
    let created = "2026-10-01T00:00:00Z";
    let fifty = repo(vec![
        pr(6, 50, created, "feat/x"),
        pr(5, 50, created, "feat/x"),
    ]);
    h.store
        .gh_replace("strate", "example", &fifty, 1)
        .expect("gh");
    let a = h.agent("s1", None);
    assert_eq!(
        charged(&a),
        (Some(issue("strate", 5)), ChargeSource::Branch)
    );
    assert_eq!(
        a.role.as_deref(),
        Some("impl"),
        "the role is still the name's"
    );

    // A refresh that no longer holds the PR gives the name back.
    h.store
        .gh_replace("strate", "example", &repo(vec![]), 2)
        .expect("gh");
    assert_eq!(
        charged(&h.agent("s1", None)),
        (Some(issue("strate", 9)), ChargeSource::Name)
    );
}

#[test]
fn a_default_detached_or_empty_branch_never_resolves() {
    let mut h = Home::new("model-default-branch");
    let created = "2026-10-01T00:00:00Z";
    let defaults = ["main", "master", "HEAD", ""];
    let prs = defaults
        .iter()
        .enumerate()
        .map(|(n, head)| pr(7, 70 + n as i64, created, head))
        .collect();
    h.store
        .gh_replace("strate", "example", &repo(prs), 1)
        .expect("gh");
    for (n, branch) in defaults.iter().enumerate() {
        let session = format!("d{n}");
        h.feed(
            &session,
            None,
            &[record("10:00:00", "/work/strate", branch)],
        );
        assert_eq!(
            charged(&h.agent(&session, None)),
            (None, ChargeSource::Unlinked),
            "{branch:?}"
        );
    }
}

#[test]
fn the_name_is_the_title_then_the_registry_name_then_a_non_bare_description() {
    let mut h = Home::new("model-names");
    h.store
        .apply_registry(&[
            listed(Kind::Interactive, "s2", Some("lead_repo_strate")),
            listed(Kind::Interactive, "s3", Some("lead_repo_other")),
        ])
        .expect("registry");
    h.feed("s2", None, &[record("10:00:00", "/work/x", "main")]);
    h.feed(
        "s3",
        None,
        &[
            title("harness_strate_t4"),
            record("10:00:00", "/work/x", "main"),
        ],
    );
    // A bare <slug>-<N> counts from a title.
    h.feed(
        "s4",
        None,
        &[title("strate-12"), record("10:00:00", "/work/x", "main")],
    );
    h.store
        .ingest_graph(&graph(vec![
            dispatched("s5", "x1", orchestrator("s5"), None, Some("fix-step-2")),
            dispatched(
                "s5",
                "x2",
                orchestrator("s5"),
                None,
                Some("review_pr_strate-3"),
            ),
            dispatched(
                "s5",
                "x3",
                orchestrator("s5"),
                Some("impl_issue_strate-8"),
                Some("review_pr_strate-3"),
            ),
        ]))
        .expect("graph");

    let s2 = h.agent("s2", None);
    let repo_strate = charge(ChargeKind::Repo, Some("strate"), None, None);
    assert_eq!(charged(&s2), (Some(repo_strate), ChargeSource::Name));
    assert_eq!(s2.role.as_deref(), Some("lead"));
    let s3 = h.agent("s3", None);
    assert_eq!(charged(&s3), (Some(issue("strate", 4)), ChargeSource::Name));
    assert_eq!(s3.role, None, "the title wins over the registry name");
    assert_eq!(
        charged(&h.agent("s4", None)),
        (Some(issue("strate", 12)), ChargeSource::Name)
    );

    let x1 = h.agent("s5", Some("x1"));
    assert_eq!(
        charged(&x1),
        (None, ChargeSource::Unlinked),
        "a bare description is no charge, and s5 has none to pass on"
    );
    assert_eq!(x1.role, None);
    let x2 = h.agent("s5", Some("x2"));
    let pr3 = charge(ChargeKind::Pr, Some("strate"), Some(3), None);
    assert_eq!(charged(&x2), (Some(pr3), ChargeSource::Name));
    assert_eq!(x2.role.as_deref(), Some("review"));
    let x3 = h.agent("s5", Some("x3"));
    assert_eq!(charged(&x3), (Some(issue("strate", 8)), ChargeSource::Name));
    assert_eq!(
        x3.role.as_deref(),
        Some("impl"),
        "the name before the description"
    );
}

#[test]
fn several_prs_on_a_branch_prefer_the_name_repo_then_the_cwd_repo_then_the_newest() {
    let mut h = Home::new("model-prs");
    let head = "fix/shared";
    let gh = [
        ("alpha", pr(10, 1, "2026-10-01T00:00:00Z", head)),
        ("beta", pr(20, 2, "2026-10-05T00:00:00Z", head)),
        ("gamma", pr(30, 3, "2026-10-05T00:00:00Z", head)),
    ];
    for (slug, p) in gh {
        h.store
            .gh_replace(slug, "example", &repo(vec![p]), 1)
            .expect("gh");
    }
    let cases = [
        // The name's repo, though the cwd names another.
        (
            "a",
            Some("impl_issue_alpha-99"),
            "/work/gamma",
            issue("alpha", 10),
        ),
        // A cwd segment led by `<repo>-`, Windows separators too.
        ("b", None, "C:\\work\\alpha-wt\\sub", issue("alpha", 10)),
        ("c", None, "/work/gamma", issue("gamma", 30)),
        // Neither: the newest, then the lowest number (beta 2 before gamma 3).
        ("d", None, "/work/x", issue("beta", 20)),
        ("e", None, "/work/alphabet", issue("beta", 20)),
    ];
    for (session, name, cwd, want) in cases {
        let mut lines: Vec<Value> = name.map(title).into_iter().collect();
        lines.push(record("10:00:00", cwd, head));
        h.feed(session, None, &lines);
        assert_eq!(
            charged(&h.agent(session, None)),
            (Some(want), ChargeSource::Branch),
            "{session}"
        );
    }
}

#[test]
fn the_repos_worth_fetching_are_those_of_the_agents_name_charges() {
    let mut h = Home::new("model-slugs");
    h.feed(
        "s1",
        None,
        &[
            title("impl_issue_alpha-5"),
            record("10:00:00", "/work/x", "fix/a"),
        ],
    );
    h.feed("s2", None, &[title("researcher_topic_pricing")]);
    h.feed("s3", None, &[title("lead_repo_beta")]);
    // s1's branch heads a gamma PR: its own charge, but not its name's.
    let gamma = repo(vec![pr(9, 1, "2026-10-01T00:00:00Z", "fix/a")]);
    h.store
        .gh_replace("gamma", "example", &gamma, 1)
        .expect("gh");
    assert_eq!(
        charged(&h.agent("s1", None)),
        (Some(issue("gamma", 9)), ChargeSource::Branch)
    );
    assert_eq!(h.store.charge_slugs().expect("slugs"), ["alpha", "beta"]);
}

#[test]
fn a_bare_name_charges_only_an_orchestrator() {
    let mut h = Home::new("model-bare");
    h.feed(
        "s1",
        None,
        &[title("strate-16"), record("10:00:00", "/work/x", "main")],
    );
    // A teammate and a subagent whose meta names are bare: worker labels
    // (`unit-3`, `recon-2`), not issues.
    let mate = SubagentNode {
        session_id: "s1".into(),
        agent_id: "t1".into(),
        path: transcript("s1", Some("t1")),
        meta: Some(SubagentMeta {
            name: Some("unit-3".into()),
            team_name: Some("team-lorem".into()),
            ..SubagentMeta::default()
        }),
        link: Link::Teammate {
            team_name: "team-lorem".into(),
            team_dir: None,
        },
    };
    let worker = dispatched("s1", "w1", orchestrator("s1"), Some("recon-2"), None);
    h.store
        .ingest_graph(&graph(vec![mate, worker]))
        .expect("graph");

    let strate16 = Some(issue("strate", 16));
    assert_eq!(
        charged(&h.agent("s1", None)),
        (strate16.clone(), ChargeSource::Name),
        "an orchestrator's bare title counts"
    );
    for agent in ["t1", "w1"] {
        assert_eq!(
            charged(&h.agent("s1", Some(agent))),
            (strate16.clone(), ChargeSource::Parent),
            "{agent}: its lead's charge, not its bare name's"
        );
    }
}

#[test]
fn a_bare_registry_name_is_no_charge_but_a_non_bare_one_is() {
    // strate has no registry nameSource, so a bare registry name may be an
    // unverified launch label (lib.mjs `isChosenName`): only the
    // orchestrator's own title is a chosen bare name.
    let mut h = Home::new("model-bare-registry");
    h.store
        .apply_registry(&[
            listed(Kind::Interactive, "s1", Some("code-42")),
            listed(Kind::Interactive, "s2", Some("impl_issue_strate-16")),
        ])
        .expect("registry");
    for (session, lines) in [
        ("s1", vec![record("10:00:00", "/work/x", "main")]),
        ("s2", vec![record("10:00:00", "/work/x", "main")]),
        (
            "s3",
            vec![title("strate-16"), record("10:00:00", "/work/x", "main")],
        ),
    ] {
        h.feed(session, None, &lines);
    }
    assert_eq!(
        charged(&h.agent("s1", None)),
        (None, ChargeSource::Unlinked),
        "a bare registry name"
    );
    let strate16 = Some(issue("strate", 16));
    assert_eq!(
        charged(&h.agent("s2", None)),
        (strate16.clone(), ChargeSource::Name),
        "a non-bare registry name"
    );
    assert_eq!(
        charged(&h.agent("s3", None)),
        (strate16, ChargeSource::Name),
        "a bare title"
    );
}

// ---- parents and the effective charge --------------------------------------------

#[test]
fn an_agent_without_its_own_charge_takes_its_nearest_spawn_ancestors() {
    let mut h = Home::new("model-inherit");
    h.feed(
        "s1",
        None,
        &[
            title("impl_issue_strate-5"),
            record("10:00:00", "/work/x", "main"),
        ],
    );
    h.store
        .ingest_graph(&graph(vec![
            dispatched("s1", "d1", orchestrator("s1"), None, None),
            dispatched("s1", "d2", subagent("s1", "d1"), None, Some("fix-step-2")),
            dispatched(
                "s1",
                "d3",
                orchestrator("s1"),
                None,
                Some("review_pr_strate-3"),
            ),
        ]))
        .expect("graph");
    // d1 messages s2 first, then s3 does.
    h.feed("s1", Some("d1"), &[sent("10:05:00", "m1")]);
    h.feed("s3", None, &[sent("10:06:00", "m2")]);
    h.feed(
        "s2",
        None,
        &[peer("10:05:02", "m1"), peer("10:06:02", "m2")],
    );
    h.feed("s4", None, &[record("10:00:00", "/work/x", "main")]);

    let s1 = h.id("s1", None);
    let d1 = h.id("s1", Some("d1"));
    let strate5 = Some(issue("strate", 5));
    let got = |session: &str, agent: Option<&str>| {
        let a = h.agent(session, agent);
        (a.charge, a.charge_source, a.parent, a.parent_via)
    };
    let dispatch = Some("dispatch".to_string());
    assert_eq!(
        got("s1", Some("d1")),
        (
            strate5.clone(),
            ChargeSource::Parent,
            Some(s1),
            dispatch.clone()
        )
    );
    assert_eq!(
        got("s1", Some("d2")),
        (
            strate5.clone(),
            ChargeSource::Parent,
            Some(d1),
            dispatch.clone()
        ),
        "two levels up, a bare description no charge of its own"
    );
    let pr3 = charge(ChargeKind::Pr, Some("strate"), Some(3), None);
    assert_eq!(
        got("s1", Some("d3")),
        (Some(pr3), ChargeSource::Name, Some(s1), dispatch),
        "its own charge before its parent's"
    );
    assert_eq!(
        got("s2", None),
        (
            strate5,
            ChargeSource::Parent,
            Some(d1),
            Some("message".into())
        ),
        "a session hangs under the subagent that messaged it first"
    );
    assert_eq!(
        got("s4", None),
        (None, ChargeSource::Unlinked, None, None),
        "no charge and no parent: Unlinked"
    );
}

#[test]
fn a_message_closing_a_cycle_through_a_dispatch_is_refused_and_the_next_messager_wins() {
    let mut h = Home::new("model-cycle");
    // s4 dispatches b1 (the graph arrives last); b1 messages s5, s5
    // messages s4 back (s4 -> b1 -> s5 -> s4 would be a cycle), then s6
    // messages s4.
    h.feed(
        "s4",
        None,
        &[
            title("director-hub"),
            record("10:00:00", "/work/hub", "main"),
        ],
    );
    h.feed("s4", Some("b1"), &[sent("10:10:00", "m2")]);
    h.feed(
        "s5",
        None,
        &[peer("10:10:02", "m2"), sent("10:20:00", "m3")],
    );
    h.feed("s6", None, &[sent("10:30:00", "m4")]);
    h.feed(
        "s4",
        None,
        &[peer("10:20:02", "m3"), peer("10:30:02", "m4")],
    );
    h.store
        .ingest_graph(&graph(vec![dispatched(
            "s4",
            "b1",
            orchestrator("s4"),
            None,
            None,
        )]))
        .expect("graph");

    let via = |session: &str, agent: Option<&str>| {
        let a = h.agent(session, agent);
        (a.parent, a.parent_via)
    };
    let s4 = h.id("s4", None);
    let b1 = h.id("s4", Some("b1"));
    let s6 = h.id("s6", None);
    assert_eq!(via("s4", Some("b1")), (Some(s4), Some("dispatch".into())));
    assert_eq!(via("s5", None), (Some(b1), Some("message".into())));
    assert_eq!(via("s4", None), (Some(s6), Some("message".into())));
    let s5 = h.agent("s5", None);
    let hub = charge(ChargeKind::Topic, None, None, Some("hub"));
    assert_eq!(charged(&s5), (Some(hub), ChargeSource::Parent));
    let s4 = h.agent("s4", None);
    assert_eq!(s4.role.as_deref(), Some("director"));
}

// ---- lanes ----------------------------------------------------------------------

#[test]
fn an_orchestrator_is_a_background_session_when_a_background_row_names_its_chain() {
    let mut h = Home::new("model-lanes");
    h.store
        .apply_registry(&[
            listed(Kind::Background, "s7", Some("impl_issue_strate-111")),
            listed(Kind::Interactive, "s9", None),
        ])
        .expect("registry");
    h.feed("s7", None, &[rooted("09:00:00", "s7")]);
    // s8 is s7's later /clear link: one process.
    h.feed("s8", None, &[rooted("11:00:00", "s7")]);
    h.feed("s9", None, &[rooted("09:00:00", "s9")]);
    h.feed("s10", None, &[rooted("09:00:00", "s10")]);
    h.store
        .ingest_graph(&graph(vec![dispatched(
            "s7",
            "w1",
            orchestrator("s7"),
            None,
            None,
        )]))
        .expect("graph");

    let lane = |session: &str, agent: Option<&str>| h.agent(session, agent).lane;
    assert_eq!(lane("s7", None), Lane::BackgroundSession);
    assert_eq!(lane("s8", None), Lane::BackgroundSession);
    assert_eq!(lane("s9", None), Lane::Interactive);
    assert_eq!(lane("s10", None), Lane::Interactive);
    assert_eq!(lane("s7", Some("w1")), Lane::Subagent);
    // The process's name charges every link of it.
    for session in ["s7", "s8"] {
        let a = h.agent(session, None);
        assert_eq!(
            charged(&a),
            (Some(issue("strate", 111)), ChargeSource::Name),
            "{session}"
        );
        assert_eq!(
            a.name.as_deref(),
            Some("impl_issue_strate-111"),
            "{session}"
        );
    }
}

// ---- model-home: cold and incremental ---------------------------------------------

const M1: &str = "5e55f1f1-0000-4000-8000-0000000000d1";
const M2: &str = "5e55f1f1-0000-4000-8000-0000000000d2";
const M3: &str = "5e55f1f1-0000-4000-8000-0000000000d3";
const M4: &str = "5e55f1f1-0000-4000-8000-0000000000d4";
const M5: &str = "5e55f1f1-0000-4000-8000-0000000000d5";
const M6: &str = "5e55f1f1-0000-4000-8000-0000000000d6";
const M7: &str = "5e55f1f1-0000-4000-8000-0000000000d7";
const M8: &str = "5e55f1f1-0000-4000-8000-0000000000d8";
const A1: &str = "a8000000000000001";
const A2: &str = "a8000000000000002";
const A3: &str = "a8000000000000003";
const B1: &str = "a8000000000000004";

fn home() -> PathBuf {
    fixture_home("model-home")
}

/// strate as `github/model-home.json` holds it.
fn strate() -> Repo {
    let json = std::fs::read(fixtures().join("github/model-home.json")).expect("gh json");
    let mut once = Some(json);
    let mut runner: Runner =
        Box::new(move |_| once.take().ok_or_else(|| io::Error::other("again")));
    fetch_repo(&mut runner, "example", "strate").expect("fetch")
}

fn registry() -> Vec<RegistryEvent> {
    vec![
        listed(Kind::Background, M7, Some("impl_issue_strate-111")),
        listed(Kind::Interactive, M3, None),
    ]
}

/// Each agent keyed by (session, agent id), its parent by the same key.
type Keyed = (
    (String, Option<String>),
    String,
    Lane,
    Option<String>,
    Option<String>,
    Option<Charge>,
    ChargeSource,
    Option<(String, Option<String>)>,
    Option<String>,
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
                a.kind.clone(),
                a.lane,
                a.role.clone(),
                a.name.clone(),
                a.charge.clone(),
                a.charge_source,
                a.parent.map(|p| keys[&p].clone()),
                a.parent_via.clone(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn cold(t: &Temp) -> Vec<ModelAgent> {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .ingest_graph(&discover(&home()).expect("discover"))
        .expect("graph");
    for path in transcripts(&home()) {
        store.ingest(&whole(&path)).expect("ingest");
    }
    store
        .gh_replace("strate", "example", &strate(), 1)
        .expect("gh");
    store.apply_registry(&registry()).expect("registry");
    drop(store);
    views::agent_model(&Store::open_reader(t.db()).expect("reader")).expect("model")
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
fn model_home_reads_as_its_generator_describes() {
    let t = Temp::new("model-home-cold");
    let model = cold(&t);
    let find = |session: &str, agent: Option<&str>| {
        model
            .iter()
            .find(|a| a.session_id == session && a.agent_id.as_deref() == agent)
            .unwrap_or_else(|| panic!("{session} {agent:?}"))
    };
    let id = |session: &str, agent: Option<&str>| find(session, agent).id;
    let row = |session: &str, agent: Option<&str>| {
        let a = find(session, agent);
        (
            a.role.as_deref(),
            a.charge.clone(),
            a.charge_source,
            a.parent,
            a.parent_via.as_deref(),
        )
    };
    let s107 = Some(issue("strate", 107));
    let hub = Some(charge(ChargeKind::Topic, None, None, Some("hub")));
    let pr120 = Some(charge(ChargeKind::Pr, Some("strate"), Some(120), None));
    use ChargeSource::{Branch, Name, Parent, Unlinked};
    let m1 = Some(id(M1, None));
    let a1 = Some(id(M1, Some(A1)));
    let m4 = Some(id(M4, None));
    assert_eq!(
        row(M1, None),
        (Some("manager"), s107.clone(), Branch, None, None)
    );
    assert_eq!(
        row(M1, Some(A1)),
        (None, s107.clone(), Parent, m1, Some("dispatch"))
    );
    assert_eq!(
        row(M1, Some(A2)),
        (None, s107.clone(), Parent, a1, Some("dispatch")),
        "linked by meta parentAgentId alone"
    );
    assert_eq!(
        row(M1, Some(A3)),
        (Some("review"), pr120, Name, m1, Some("dispatch"))
    );
    assert_eq!(row(M2, None), (None, s107, Parent, a1, Some("message")));
    assert_eq!(
        row(M3, None),
        (None, Some(issue("strate", 110)), Name, None, None)
    );
    assert_eq!(
        row(M4, None),
        (
            Some("director"),
            hub.clone(),
            Name,
            Some(id(M3, None)),
            Some("message")
        )
    );
    assert_eq!(
        row(M4, Some(B1)),
        (None, hub.clone(), Parent, m4, Some("dispatch"))
    );
    assert_eq!(
        row(M5, None),
        (None, hub, Parent, Some(id(M4, Some(B1))), Some("message"))
    );
    assert_eq!(row(M6, None), (None, None, Unlinked, None, None));
    for session in [M7, M8] {
        assert_eq!(find(session, None).lane, Lane::BackgroundSession);
        assert_eq!(
            row(session, None),
            (Some("impl"), Some(issue("strate", 111)), Name, None, None)
        );
    }
    assert_eq!(find(M3, None).lane, Lane::Interactive);
    assert_eq!(find(M1, Some(A1)).lane, Lane::Subagent);
}

#[test]
fn an_incremental_build_in_any_order_equals_a_cold_one() {
    let ct = Temp::new("model-cold");
    let want = keyed(&cold(&ct));

    // GitHub and the registry first, then every transcript's first half in
    // reverse order, the graph, and the second halves in order.
    let t = Temp::new("model-incremental");
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    store
        .gh_replace("strate", "example", &strate(), 1)
        .expect("gh");
    store.apply_registry(&registry()).expect("registry");
    let paths = transcripts(&home());
    let halves: Vec<usize> = paths
        .iter()
        .map(|p| records(p, 0..usize::MAX).len().div_ceil(2))
        .collect();
    for (path, half) in paths.iter().zip(&halves).rev() {
        if let Some(b) = part(path, 0..*half) {
            store.ingest(&b).expect("first half");
        }
    }
    store
        .ingest_graph(&discover(&home()).expect("discover"))
        .expect("graph");
    for (path, half) in paths.iter().zip(&halves) {
        if let Some(b) = part(path, *half..usize::MAX) {
            store.ingest(&b).expect("second half");
        }
    }
    let reader = Store::open_reader(t.db()).expect("reader");
    assert_eq!(keyed(&views::agent_model(&reader).expect("model")), want);

    // A rebuild from the stored rows changes nothing.
    store.rebuild_model().expect("rebuild");
    assert_eq!(keyed(&views::agent_model(&reader).expect("model")), want);
}

#[test]
fn the_model_serializes_in_the_webview_shape() {
    let mut h = Home::new("model-json");
    h.feed(
        "s1",
        None,
        &[
            title("impl_issue_strate-5"),
            record("10:00:00", "/work/x", "main"),
        ],
    );
    let json = serde_json::to_value(h.agent("s1", None)).expect("json");
    let mut keys: Vec<&str> = json
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "agentId",
            "charge",
            "chargeSource",
            "id",
            "kind",
            "lane",
            "name",
            "parent",
            "parentVia",
            "role",
            "sessionId"
        ]
    );
    assert_eq!(json["lane"], "interactive");
    assert_eq!(json["chargeSource"], "name");
    assert_eq!(json["charge"]["kind"], "issue");
    assert_eq!(
        serde_json::to_value(Lane::BackgroundSession).expect("json"),
        "background-session"
    );
}
