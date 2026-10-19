//! Teammates that went idle (#83), over the synthetic
//! `tests/fixtures/teams-home`. A teammate ends each turn with an
//! idle_notification teammate-message in its lead's transcript; one whose
//! newest notice (or TeammateIdle hook) is at or after its own last user
//! or assistant record reads done, so the live graph leaves it out (#81).
//! Its next record makes it work again. Every db lives in a temp dir.

use std::ops::Range;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use strate_core::discovery::discover;
use strate_core::hooks::{Hook, HookEvent};
use strate_core::registry::{RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::store::views::{self, AgentState, AuditFilter};
use strate_core::tail::Batch;
use strate_core::time::parse_timestamp;

mod common;

use common::{Temp, batch, fixture_home, records};

/// team-repo-1, the team lead.
const TL: &str = "5e55d5d5-0000-4000-8000-0000000000a1";

fn home() -> PathBuf {
    fixture_home("teams-home")
}

fn lead() -> PathBuf {
    home().join(format!("projects/-work-team-repo/{TL}.jsonl"))
}

/// Teammate `n`'s transcript: 1 lorem-mate, 2 ipsum-mate, 3 dolor-mate,
/// 4 amet-mate.
fn mate(n: u8) -> PathBuf {
    home().join(format!(
        "projects/-work-team-repo/{TL}/subagents/agent-a700000000000000{n}.jsonl"
    ))
}

fn agent_id(n: u8) -> String {
    format!("a700000000000000{n}")
}

/// Lines `lines` of `path` as one batch, with no checkpoint.
fn part(path: &Path, lines: Range<usize>) -> Batch {
    Batch {
        path: path.into(),
        events: records(path, lines).into_iter().map(|(e, _)| e).collect(),
        checkpoint: None,
    }
}

fn ms(stamp: &str) -> i64 {
    parse_timestamp(stamp).expect("a transcript timestamp")
}

/// One of the steps a store takes in: the discovery graph, or a batch.
enum Step {
    Graph,
    Ingest(Batch),
}

/// A store in UTC that took `steps` in order, its lead listed live, and a
/// reader on it.
fn build(t: &Temp, steps: Vec<Step>) -> (Store, Connection) {
    let mut store = Store::open_in(t.db(), "UTC").expect("open");
    for step in steps {
        match step {
            Step::Graph => store
                .ingest_graph(&discover(&home()).expect("discover"))
                .expect("graph"),
            Step::Ingest(b) => store.ingest(&b).expect("ingest"),
        }
    }
    let entry = parse_snapshot(
        format!(
            r#"[{{"pid":4301,"cwd":"/work/team-repo","kind":"interactive",
                 "startedAt":1791000000000,"sessionId":"{TL}","name":"team-repo-1",
                 "status":"busy"}}]"#
        )
        .as_bytes(),
    )
    .expect("entry")
    .remove(0);
    store
        .apply_registry(&[RegistryEvent::Changed { entry, at_ms: 5000 }])
        .expect("listed");
    let reader = Store::open_reader(t.db()).expect("reader");
    (store, reader)
}

/// The graph first, then every transcript whole.
fn cold() -> Vec<Step> {
    let mut steps = vec![Step::Graph, Step::Ingest(batch(&lead()))];
    steps.extend((1..=4).map(|n| Step::Ingest(batch(&mate(n)))));
    steps
}

/// The drawn teammates' names and states, by name.
fn drawn(db: &Connection) -> Vec<(String, AgentState)> {
    let rows = views::workstreams(db).expect("workstreams");
    let tab = rows
        .iter()
        .find(|w| w.name == "team-repo-1")
        .unwrap_or_else(|| panic!("no team-repo-1 tab: {rows:?}"));
    let graph = views::live_graph(db, tab.id)
        .expect("query")
        .expect("a graph");
    assert_eq!(graph.agents[0].kind, "orchestrator", "{graph:?}");
    let mut mates: Vec<(String, AgentState)> = graph
        .agents
        .iter()
        .filter(|a| a.kind == "teammate")
        .map(|a| (a.name.clone().expect("a teammate name"), a.state))
        .collect();
    mates.sort_by(|a, b| a.0.cmp(&b.0));
    mates
}

fn working(names: &[&str]) -> Vec<(String, AgentState)> {
    let mut out: Vec<_> = names
        .iter()
        .map(|n| (n.to_string(), AgentState::Working))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn a_teammate_is_typed_as_it_was_spawned_else_teammate() {
    // spawnedAgentType, else customAgentType; neither: `teammate` (#86).
    let t = Temp::new("teammate-types");
    let (_store, db) = build(&t, cold());
    let types: Vec<(String, String)> = db
        .prepare("SELECT name, agent_type FROM agents WHERE kind = 'teammate' ORDER BY agent_id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let pair = |n: &str, ty: &str| (n.to_string(), ty.to_string());
    assert_eq!(
        types,
        [
            pair("lorem-mate", "implementer"),
            pair("ipsum-mate", "general-purpose"),
            pair("dolor-mate", "reviewer"),
            pair("amet-mate", "teammate"),
        ]
    );
}

#[test]
fn the_performance_agents_table_shows_teammates_by_their_spawned_type() {
    let t = Temp::new("teammate-performance");
    let (_store, db) = build(&t, cold());
    let filter = AuditFilter {
        from: "2026-10-09".into(),
        to: "2026-10-09".into(),
        repos: vec![],
    };
    let p = views::performance(&db, &filter).expect("performance");
    let mut rows: Vec<(&str, bool, i64)> = p
        .agents
        .iter()
        .map(|a| (a.agent_type.as_str(), a.orchestrator, a.dispatches))
        .collect();
    rows.sort_unstable();
    assert_eq!(
        rows,
        [
            ("general-purpose", false, 1),
            ("implementer", false, 1),
            ("orchestrator", true, 0),
            ("reviewer", false, 1),
            ("teammate", false, 1),
        ]
    );
}

#[test]
fn a_teammate_whose_newest_notice_follows_its_last_record_is_not_drawn() {
    let t = Temp::new("teams-cold");
    let (_store, db) = build(&t, cold());
    // lorem-mate idled after its last record. ipsum-mate was messaged
    // again and worked on; dolor-mate never idled; amet-mate worked again
    // after its notice's own time, though the lead read it later.
    assert_eq!(
        drawn(&db),
        working(&["amet-mate", "dolor-mate", "ipsum-mate"])
    );
}

#[test]
fn every_ingest_order_reads_as_the_cold_build() {
    let t = Temp::new("teams-cold-ref");
    let (_store, db) = build(&t, cold());
    let want = drawn(&db);
    let mates = || (1..=4).map(|n| Step::Ingest(batch(&mate(n))));
    let orders: Vec<Vec<Step>> = vec![
        // The lead before its teammates, then the graph.
        std::iter::once(Step::Ingest(batch(&lead())))
            .chain(mates())
            .chain([Step::Graph])
            .collect(),
        // The teammates before the lead.
        std::iter::once(Step::Graph)
            .chain(mates())
            .chain([Step::Ingest(batch(&lead()))])
            .collect(),
        // The lead split around its notices, the teammates between.
        std::iter::once(Step::Graph)
            .chain([Step::Ingest(part(&lead(), 0..4))])
            .chain(mates())
            .chain([Step::Ingest(part(&lead(), 4..usize::MAX))])
            .collect(),
    ];
    for (i, steps) in orders.into_iter().enumerate() {
        let t = Temp::new(&format!("teams-order-{i}"));
        let (_store, db) = build(&t, steps);
        assert_eq!(drawn(&db), want, "order {i}");
    }
}

#[test]
fn a_notice_hides_a_teammate_and_its_next_record_draws_it_again() {
    let t = Temp::new("teams-wake");
    // The lead's title and first turn, every teammate but ipsum-mate's
    // second turn; then the lead's 10:05:01 record, with lorem-mate's and
    // ipsum-mate's notices.
    let (mut store, db) = build(
        &t,
        vec![
            Step::Graph,
            Step::Ingest(part(&lead(), 0..3)),
            Step::Ingest(batch(&mate(1))),
            Step::Ingest(part(&mate(2), 0..2)),
            Step::Ingest(batch(&mate(3))),
            Step::Ingest(batch(&mate(4))),
        ],
    );
    let all = ["amet-mate", "dolor-mate", "ipsum-mate", "lorem-mate"];
    assert_eq!(drawn(&db), working(&all));
    store.ingest(&part(&lead(), 3..4)).expect("notices");
    assert_eq!(
        drawn(&db),
        working(&["amet-mate", "dolor-mate"]),
        "both idle after their last records"
    );
    // The lead messages ipsum-mate again: its next record wakes it.
    store.ingest(&part(&mate(2), 2..3)).expect("woken");
    assert_eq!(
        drawn(&db),
        working(&["amet-mate", "dolor-mate", "ipsum-mate"])
    );
}

#[test]
fn a_teammate_idle_hook_reads_as_a_notice() {
    let t = Temp::new("teams-hook");
    // No notice in the store: the lead's title and first turn only.
    let (mut store, db) = build(
        &t,
        vec![
            Step::Graph,
            Step::Ingest(part(&lead(), 0..3)),
            Step::Ingest(batch(&mate(1))),
            Step::Ingest(part(&mate(2), 0..2)),
            Step::Ingest(batch(&mate(3))),
            Step::Ingest(batch(&mate(4))),
        ],
    );
    let idle = |n: u8, at: &str| Hook {
        received_ms: ms(at),
        event: HookEvent::TeammateIdle {
            session_id: TL.into(),
            agent_id: Some(agent_id(n)),
            teammate_name: None,
        },
    };
    store
        .apply_hook(&idle(2, "2026-10-09T10:04:00.030Z"))
        .expect("ipsum-mate idle");
    // Stale: dolor-mate's last record is 10:06:00.
    store
        .apply_hook(&idle(3, "2026-10-09T10:05:00.000Z"))
        .expect("dolor-mate idle before its last record");
    assert_eq!(
        drawn(&db),
        working(&["amet-mate", "dolor-mate", "lorem-mate"])
    );
    store.ingest(&part(&mate(2), 2..4)).expect("woken");
    assert_eq!(
        drawn(&db),
        working(&["amet-mate", "dolor-mate", "ipsum-mate", "lorem-mate"])
    );
}

#[test]
fn a_notice_does_not_hide_a_teammate_with_no_record_yet() {
    let t = Temp::new("teams-unread");
    // lorem-mate's notice is stored but not its transcript yet: a notice
    // matched by name alone may be an earlier teammate's of that name.
    let mut steps = vec![Step::Graph, Step::Ingest(batch(&lead()))];
    steps.extend((2..=4).map(|n| Step::Ingest(batch(&mate(n)))));
    let (_store, db) = build(&t, steps);
    assert_eq!(
        drawn(&db),
        working(&["amet-mate", "dolor-mate", "ipsum-mate", "lorem-mate"])
    );
}
