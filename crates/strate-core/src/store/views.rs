//! Read views for the app: the live tabs, needs-you, per-tab $, the live
//! graph, the agent model ([`agent_model`]), and the Audit rollups, row
//! timeline and performance read ([`audit()`], [`audit_timeline()`],
//! [`performance()`], from `store/audit/`). Each runs on a read-only
//! connection from [`super::Store::open_reader`] and serializes in camelCase
//! for the webview.
//!
//! A registry row describes its process's `/clear` chain's latest link
//! (#63): `claude agents` keys a background process by the session it
//! started with, so after a `/clear` the row still names the first link
//! while the latest one runs. A session is live when a registry row of its
//! chain that is running makes it that latest link ([`listings`]), and
//! its orchestrator's registry name, state and wait come from that row. An
//! earlier link is never live. A background session `blocked` on the user
//! is not running either: no live tab and no unit node, only a needs-you
//! ([`needs_you`]) on its latest link. A hook's needs-you is on a latest
//! link only (#68): a `/clear` left an earlier link's prompt behind. A
//! registry wait and a hook wait on one stop are one needs-you row, the
//! hook's (#84).
//!
//! A tab ([`Workstream`]) is one `/clear` chain: the sessions of one
//! process root with orchestrator records, its links. Its id is its first
//! link's orchestrator's row id (any link's orchestrator id reads as the
//! tab in [`live_graph`]), and it draws one link: a live one first, then
//! the latest start. It is named by that link's orchestrator: its effective
//! charge's key (the `agent_charges` rule), else its name, its registry
//! name, its cwd and branch, its session id; a tab with no charge is
//! `fallback` (Unlinked). Its $ is every request of its links, every
//! agent's.
//!
//! Tabs form the company tree (#109) by the model's spawn parents
//! (`spawn_parents`, whatever the edge kind): a chain's spawner is the
//! parent of the earliest of its links' orchestrators whose parent is an
//! agent of another live tab, else of another tab, and that tab is its
//! parent tab. A tab's root
//! ([`Workstream::director`]) is the topmost live tab of its spawn
//! ancestry, past any that is not live; a chain with no live ancestor keeps
//! its own tab, and so does one in a cycle of spawners, so no tab hides
//! another. Only a live tab with no root shows in the strip. Every
//! needs-you row names its session's own chain's tab, whose `director`
//! leads to the root tab, and a tab's needs-you and state count every tab
//! nested under it, at any depth.
//!
//! A tab's live graph is the whole running tree beneath it: its drawn
//! link's orchestrator and the drawn link of every live chain whose spawn
//! ancestry holds it, each with its subagents and teammates that still run.
//! A worker hangs under its dispatcher or team lead (dispatch and teammate
//! edges); a chain's orchestrator under its spawner (kind `directs` for a
//! `message` spawn, else the spawn's own kind). A worker that reads done is
//! not drawn (#81): finished workers are the Audit's. A teammate reads done
//! while it rests (#83): its newest turn end, an idle notice in its lead's
//! transcript or a TeammateIdle hook, is at or after its own last user or
//! assistant record. A worker that hangs under an earlier link's
//! orchestrator of its own chain (a teammate's lead can be another link's)
//! hangs under that chain's drawn one. Any agent whose parent is not drawn
//! (finished, an earlier link's, a chain that is not live) hangs under its
//! nearest drawn spawn ancestor, else the tab's orchestrator. The header's
//! $ and run time sum the tab's drawn link and the links before it (#65).
//! Each drawn agent carries its model reading ([`agent_model`]): session,
//! role, lane and effective charge.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use super::Result;
pub use super::audit::{
    AgentRow, Audit, AuditFilter, AuditNode, Bottleneck, BottleneckDay, GithubError, Headline,
    LaneCount, Metrics, NodeKind, Outcome, OutcomeDay, Peak, Performance, PermissionWaits,
    Progress, SpanKind, TimelineDay, TimelineMark, TimelineSpan, Tokens, Waste, audit,
    audit_timeline, performance,
};
pub use super::model::{ChargeSource, Lane, ModelAgent, agent_model};
use super::model::{FINISHED_SQL, effective_charge, lane};
pub use super::transcript::{
    ARGS_MAX, RESULT_MAX, TranscriptBody, TranscriptEntry, TranscriptPage, TranscriptResult,
    TranscriptTodo, TranscriptTodoStatus, TranscriptToolStatus, agent_transcript,
};
use crate::charge::Charge;
use crate::time::parse_timestamp;

/// An agent's state, as the canvas and tabs show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    NeedsYou,
    Idle,
    Done,
}

/// One tab: a `/clear` chain.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workstream {
    /// The chain's first link's orchestrator's row id.
    pub id: i64,
    /// The drawn link's orchestrator's charge key, else its name.
    pub name: String,
    /// The drawn link's orchestrator has no charge: it is Unlinked.
    pub fallback: bool,
    /// Sum of the chain's priced requests; `None` with none.
    pub cost_usd: Option<f64>,
    pub unpriced: i64,
    /// Needs-you when anything in it (or nested under it, at any depth)
    /// waits on the user, else the most urgent of the drawn link's
    /// orchestrator state and its nested tabs' states.
    pub state: AgentState,
    /// Waits in it and in the tabs nested under it, at any depth.
    pub needs_you: i64,
    /// The session [`live_graph`] draws.
    pub session_id: Option<String>,
    /// Whether that session is live: the latest link of a process the
    /// registry lists as running.
    pub live: bool,
    /// The chain's latest link start.
    pub started_at: Option<String>,
    /// Its root: the topmost live tab of its spawn ancestry, which it nests
    /// under; `None` on a root or a tab with no live ancestor.
    pub director: Option<i64>,
}

/// One tab's $.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkstreamCost {
    /// The tab's id.
    pub workstream: i64,
    pub cost_usd: Option<f64>,
    pub unpriced: i64,
}

/// One row of the `needs_you` view and the tab it counts toward.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeedsYou {
    /// `registry` or `hook`.
    pub source: String,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub waiting_for: Option<String>,
    pub since_ms: Option<i64>,
    /// The tab of the session's own chain, at any depth; that tab's
    /// [`Workstream::director`] is the root tab it counts toward.
    pub workstream: Option<i64>,
    /// A parked background job's short id in the agent registry (`claude
    /// agents --json`), the `<id>` of `claude attach <id>` (#120); `None`
    /// on every other wait, and on a parked job the registry gave no id.
    pub bg_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveGraph {
    pub session_id: String,
    pub live: bool,
    /// The priced requests of the drawn link and the chain's links before
    /// it, every agent's; `None` with none.
    pub session_cost_usd: Option<f64>,
    /// Those links' wall times summed, each first record to last
    /// (`sessions.run_ms`).
    pub run_ms: Option<i64>,
    pub agents: Vec<GraphAgent>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphAgent {
    /// The agent's row id; edges and `parent` refer to it.
    pub id: i64,
    /// `None` on the orchestrator.
    pub agent_id: Option<String>,
    /// orchestrator | subagent | teammate.
    pub kind: String,
    /// The registry name, else the transcript's latest name (or a
    /// teammate's meta name).
    pub name: Option<String>,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub state: AgentState,
    pub cost_usd: Option<f64>,
    /// The latest request's depth.
    pub context_tokens: Option<i64>,
    /// That depth over the model's window; `None` for an unknown window.
    pub context_pct: Option<f64>,
    /// The dispatching or team-lead agent, or for a spawned chain's
    /// orchestrator its spawner, else their nearest drawn ancestor.
    pub parent: Option<i64>,
    /// The agent row's session.
    pub session_id: String,
    /// The role its name gives, if any.
    pub role: Option<String>,
    pub lane: Lane,
    /// The effective charge (`agent_charges`); `None` is Unlinked.
    pub charge: Option<Charge>,
    /// That charge's key, e.g. `issue_strate-109`.
    pub charge_key: Option<String>,
    pub charge_source: ChargeSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    /// dispatch | teammate | directs (a spawned chain's orchestrator under
    /// its messager), or another spawn kind's own name.
    pub kind: String,
    pub parent: i64,
    pub child: i64,
}

/// What the registry says of one session: the newest registry row whose
/// process's `/clear` chain has it as its latest link, a live row first.
struct Listing {
    /// The row is running: not gone, and not blocked on the user.
    live: bool,
    /// Its status or background state, `gone` once gone.
    state: Option<String>,
    name: Option<String>,
    /// It waits on the user: a registry row of the `needs_you` view.
    waiting: bool,
}

/// A session no registry row describes.
const NOT_LISTED: Listing = Listing {
    live: false,
    state: None,
    name: None,
    waiting: false,
};

/// Every session the registry describes, by id: each row counts for its
/// chain's latest link (the `latest_links` view), whichever link it names.
fn listings(conn: &Connection) -> Result<HashMap<String, Listing>> {
    let mut out = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT ifnull(l.latest, r.session_id), r.gone = 0 AND r.state IS NOT 'blocked',
             CASE WHEN r.gone = 1 THEN 'gone' ELSE coalesce(r.status, r.state) END, r.name,
             r.id IN (SELECT registry FROM needs_you WHERE source = 'registry')
         FROM registry AS r LEFT JOIN latest_links AS l ON l.session_id = r.session_id
         WHERE r.session_id IS NOT NULL
         ORDER BY r.gone, r.last_seen_ms DESC, r.id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Listing {
                live: r.get(1)?,
                state: r.get(2)?,
                name: r.get(3)?,
                waiting: r.get(4)?,
            },
        ))
    })?;
    for row in rows {
        let (session_id, listing) = row?;
        out.entry(session_id).or_insert(listing);
    }
    Ok(out)
}

/// Whether `session_id` is live (see [`listings`]).
fn is_live(listings: &HashMap<String, Listing>, session_id: &str) -> bool {
    listings.get(session_id).is_some_and(|l| l.live)
}

/// One link of a `/clear` chain: a session with orchestrator records.
struct Link {
    session_id: String,
    started_at: Option<String>,
    /// Its orchestrator's row id.
    orchestrator: i64,
}

/// A `/clear` chain with a tab: its links in start order (an unstarted one
/// last), never empty. [`Tabs::by_root`] keys it by its process root.
struct Chain {
    links: Vec<Link>,
}

impl Chain {
    /// The tab's id: the first link's orchestrator.
    fn id(&self) -> i64 {
        self.links[0].orchestrator
    }

    /// The link the tab draws and whether it is live: a live one first,
    /// then the latest start, then the lowest session id.
    fn drawn(&self, listings: &HashMap<String, Listing>) -> (&Link, bool) {
        let mut best = (&self.links[0], is_live(listings, &self.links[0].session_id));
        for link in &self.links[1..] {
            let live = is_live(listings, &link.session_id);
            if (live, &link.started_at) > (best.1, &best.0.started_at) {
                best = (link, live);
            }
        }
        best
    }

    /// The latest link start.
    fn started_at(&self) -> Option<String> {
        self.links.iter().filter_map(|l| l.started_at.clone()).max()
    }
}

/// Every chain with a tab, and where each of their sessions and
/// orchestrators is.
struct Tabs {
    chains: Vec<Chain>,
    by_session: HashMap<String, usize>,
    by_orchestrator: HashMap<i64, usize>,
    by_root: HashMap<String, usize>,
}

impl Tabs {
    fn of_session(&self, session_id: &str) -> Option<&Chain> {
        self.by_session.get(session_id).map(|&i| &self.chains[i])
    }
}

/// [`tabs`]' query: each session with orchestrator records, by process
/// root (else its own id), in start order.
const LINKS_SQL: &str = "SELECT ifnull(s.process_root, s.session_id), s.session_id, s.started_at,
         a.id
     FROM sessions AS s JOIN agents AS a ON a.session_id = s.session_id AND a.agent_id IS NULL
     WHERE EXISTS (SELECT 1 FROM events AS e
         WHERE e.session_id = s.session_id AND e.agent_id IS NULL)
     ORDER BY 1, s.started_at IS NULL, s.started_at, s.session_id";

fn tabs(conn: &Connection) -> Result<Tabs> {
    let mut tabs = Tabs {
        chains: Vec::new(),
        by_session: HashMap::new(),
        by_orchestrator: HashMap::new(),
        by_root: HashMap::new(),
    };
    let mut stmt = conn.prepare_cached(LINKS_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Link {
                session_id: r.get(1)?,
                started_at: r.get(2)?,
                orchestrator: r.get(3)?,
            },
        ))
    })?;
    for row in rows {
        let (root, link) = row?;
        let at = *tabs.by_root.entry(root).or_insert_with(|| {
            tabs.chains.push(Chain { links: Vec::new() });
            tabs.chains.len() - 1
        });
        tabs.by_session.insert(link.session_id.clone(), at);
        tabs.by_orchestrator.insert(link.orchestrator, at);
        tabs.chains[at].links.push(link);
    }
    Ok(tabs)
}

/// An orchestrator's state from its registry state, unless it waits on the
/// user. `waiting` with nothing named to wait for is idle.
fn orchestrator_state(needs_you: bool, registry_state: Option<&str>) -> AgentState {
    match (needs_you, registry_state) {
        (true, _) => AgentState::NeedsYou,
        (_, None | Some("gone")) => AgentState::Done,
        (_, Some("busy" | "running")) => AgentState::Working,
        _ => AgentState::Idle,
    }
}

/// A subagent's or teammate's state: hooks first, then whether it is done
/// (finished by the model's rule, [`FINISHED_SQL`], or resting: see
/// [`session_agents`]) and whether its session is live. A session that is
/// not live reads done too, though its workers need not have finished: an
/// earlier `/clear` link, a background session blocked on the user, or one
/// the registry has not listed yet. An idle hook is a turn end, which
/// `done` covers while no newer record woke the teammate.
fn worker_state(needs_you: bool, hook: Option<&str>, done: bool, live: bool) -> AgentState {
    match (needs_you, hook) {
        (true, _) => AgentState::NeedsYou,
        (_, Some("done")) => AgentState::Done,
        _ if done || !live => AgentState::Done,
        _ => AgentState::Working,
    }
}

/// Whether a teammate rests, which reads done (#83, user: "Idle = Done,
/// hidden"): its newest turn end, an idle notice to its lead or a
/// TeammateIdle hook, is at or after its own last user or assistant
/// record. The record the lead's next message brings wakes it. A notice
/// is matched by name alone, so it may be an earlier teammate's of that
/// name: one before the teammate's first record (and so before its last)
/// never counts, and with no record yet nothing does.
fn rests(idle_ms: Option<i64>, last_record_ms: Option<i64>) -> bool {
    matches!((idle_ms, last_record_ms), (Some(idle), Some(last)) if idle >= last)
}

/// The newest idle notice per teammate name in `session_id`'s own
/// transcript, read from its stored records ([`IDLE_NOTICES_SQL`]).
fn idle_notices(conn: &Connection, session_id: &str) -> Result<HashMap<String, i64>> {
    let mut newest: HashMap<String, i64> = HashMap::new();
    let mut stmt = conn.prepare_cached(IDLE_NOTICES_SQL)?;
    for raw in stmt.query_map([session_id], |r| r.get::<_, String>(0))? {
        let Ok(record) = serde_json::from_str(&raw?) else {
            continue;
        };
        for (name, at) in notices_in(&record) {
            let at_ms = newest.entry(name).or_insert(at);
            *at_ms = (*at_ms).max(at);
        }
    }
    Ok(newest)
}

/// [`idle_notices`]' query: the user records an orchestrator wrote that
/// can hold one, by the `events_idle_notices` partial index. Named, since
/// the planner would take `events_agent` and read every record's text.
const IDLE_NOTICES_SQL: &str = "SELECT raw FROM events INDEXED BY events_idle_notices
     WHERE session_id = ?1 AND type = 'user' AND agent_id IS NULL
       AND instr(raw, 'idle_notification') > 0";

const TEAMMATE_MESSAGE: &str = "<teammate-message";
const TEAMMATE_MESSAGE_END: &str = "</teammate-message>";

/// The sender and time of each idle notice in a lead's user record: a
/// teammate-message in its text whose body, the JSON after the opening
/// tag, is an `idle_notification`. The time is the body's own, stamped at
/// the teammate's turn end; the record's is when the lead read it, which
/// can be after the teammate was woken again.
fn notices_in(record: &serde_json::Value) -> Vec<(String, i64)> {
    let content = &record["message"]["content"];
    let texts: Vec<&str> = match content.as_array() {
        Some(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        None => content.as_str().into_iter().collect(),
    };
    let mut out = Vec::new();
    for text in texts {
        let mut rest = text;
        while let Some(at) = rest.find(TEAMMATE_MESSAGE) {
            let Some(body) = after_tag(&rest[at + TEAMMATE_MESSAGE.len()..]) else {
                break;
            };
            // A JSON body's text can hold anything, a closing tag included,
            // so it ends where its JSON does. A plain message ends at its
            // closing tag, and a notice quoted in it is not one.
            let body = body.trim_start();
            let mut values =
                serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>();
            let Some(Ok(message @ serde_json::Value::Object(_))) = values.next() else {
                match body.find(TEAMMATE_MESSAGE_END) {
                    Some(end) => rest = &body[end + TEAMMATE_MESSAGE_END.len()..],
                    None => break,
                }
                continue;
            };
            rest = &body[values.byte_offset()..];
            if message["type"] == "idle_notification"
                && let (Some(from), Some(at_ms)) = (
                    message["from"].as_str(),
                    message["timestamp"].as_str().and_then(parse_timestamp),
                )
            {
                out.push((from.to_string(), at_ms));
            }
        }
    }
    out
}

/// What follows the `>` that ends an opening tag, past any `>` inside a
/// quoted attribute value.
fn after_tag(tag: &str) -> Option<&str> {
    let mut quoted = false;
    for (i, c) in tag.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '>' if !quoted => return Some(&tag[i + 1..]),
            _ => {}
        }
    }
    None
}

/// Of two states, the one a tab should show: needs-you, then working, then
/// idle, then done.
fn more_urgent(a: AgentState, b: AgentState) -> AgentState {
    let rank = |s| match s {
        AgentState::Done => 0,
        AgentState::Idle => 1,
        AgentState::Working => 2,
        AgentState::NeedsYou => 3,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// The tabs' spawn tree, by tab id.
struct Nest {
    /// Each tab with a parent tab: its spawn ancestry, nearest first,
    /// ending before the walk would repeat a tab or reach it again.
    up: HashMap<i64, Vec<i64>>,
    /// Each nested tab's root: the topmost (last) live tab of its
    /// ancestry. A tab in a cycle of spawners has none.
    roots: HashMap<i64, i64>,
}

/// The spawn tree of `parents` (tab to parent tab), with `live` telling
/// which tabs are. A root never has one itself: the walk from a tab passes
/// its root and everything above it, and a cycle member has none. The
/// result does not depend on the map's order.
fn nest(parents: &HashMap<i64, i64>, live: impl Fn(i64) -> bool) -> Nest {
    let mut nest = Nest {
        up: HashMap::new(),
        roots: HashMap::new(),
    };
    for &tab in parents.keys() {
        let mut up = Vec::new();
        let mut cyclic = false;
        let mut at = tab;
        while let Some(&parent) = parents.get(&at) {
            cyclic = parent == tab;
            if cyclic || up.contains(&parent) {
                break;
            }
            up.push(parent);
            at = parent;
        }
        if !cyclic && let Some(&root) = up.iter().rev().find(|&&t| live(t)) {
            nest.roots.insert(tab, root);
        }
        nest.up.insert(tab, up);
    }
    nest
}

/// [`tree`]'s query: each orchestrator's spawn parent, by the model's
/// `spawn_parents`, with the edge's kind and the parent's session.
const SPAWNERS_SQL: &str = "SELECT p.child, p.parent, p.kind, a.session_id
     FROM spawn_parents AS p JOIN agents AS a ON a.id = p.parent
     JOIN agents AS c ON c.id = p.child
     WHERE c.agent_id IS NULL";

/// The tabs' spawn tree, and each nested tab's spawner.
struct Tree {
    /// A tab's spawner and the spawn's kind: the parent of the earliest of
    /// its links' orchestrators whose parent is an agent of another tab
    /// that is live, else of another tab.
    spawners: HashMap<i64, (i64, String)>,
    nest: Nest,
}

/// Every tab's spawner, parent tab (the spawner's) and root ([`nest`]).
fn tree(conn: &Connection, tabs: &Tabs, listings: &HashMap<String, Listing>) -> Result<Tree> {
    let spawned: HashMap<i64, (i64, String, String)> = conn
        .prepare_cached(SPAWNERS_SQL)?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?
        .collect::<Result<_>>()?;
    let live: HashSet<i64> = tabs
        .chains
        .iter()
        .filter(|c| c.drawn(listings).1)
        .map(Chain::id)
        .collect();
    let mut spawners = HashMap::new();
    let mut parents = HashMap::new();
    for chain in &tabs.chains {
        let candidates: Vec<(i64, (i64, String))> = chain
            .links
            .iter()
            .filter_map(|link| {
                let (spawner, kind, session) = spawned.get(&link.orchestrator)?;
                let tab = tabs.of_session(session)?.id();
                (tab != chain.id()).then(|| (tab, (*spawner, kind.clone())))
            })
            .collect();
        let chosen = candidates.iter().find(|(tab, _)| live.contains(tab));
        if let Some((tab, spawner)) = chosen.or(candidates.first()) {
            parents.insert(chain.id(), *tab);
            spawners.insert(chain.id(), spawner.clone());
        }
    }
    Ok(Tree {
        spawners,
        nest: nest(&parents, |t| live.contains(&t)),
    })
}

/// Adds each nested tab's own needs-you count and state to every tab of
/// its ancestry up to its root, so a wait shows on each tab it nests under.
fn fold_nested(rows: &mut [Workstream], nest: &Nest) {
    let at: HashMap<i64, usize> = rows.iter().enumerate().map(|(i, w)| (w.id, i)).collect();
    let own: Vec<(i64, i64, AgentState)> =
        rows.iter().map(|w| (w.id, w.needs_you, w.state)).collect();
    for (id, needs, state) in own {
        let (Some(root), Some(up)) = (nest.roots.get(&id), nest.up.get(&id)) else {
            continue;
        };
        for tab in up {
            if let Some(&i) = at.get(tab) {
                rows[i].needs_you += needs;
                rows[i].state = more_urgent(rows[i].state, state);
            }
            if tab == root {
                break;
            }
        }
    }
}

/// [`chain_costs`]' query: $ per process root (else session id), from the
/// request index.
const CHAIN_COSTS_SQL: &str =
    "SELECT ifnull(s.process_root, s.session_id), sum(r.cost_usd), count(*) - count(r.cost_usd)
     FROM requests AS r JOIN sessions AS s ON s.session_id = r.session_id
     GROUP BY 1";

/// $ and unpriced requests per tab id.
fn chain_costs(conn: &Connection, tabs: &Tabs) -> Result<HashMap<i64, (Option<f64>, i64)>> {
    let mut out = HashMap::new();
    let mut stmt = conn.prepare_cached(CHAIN_COSTS_SQL)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?;
    for row in rows {
        let (root, cost) = row?;
        if let Some(&at) = tabs.by_root.get(&root) {
            out.insert(tabs.chains[at].id(), cost);
        }
    }
    Ok(out)
}

/// A tab's name and whether it is Unlinked: the effective charge key of
/// `link`'s orchestrator, else its name, else its listing's, else its cwd
/// and branch, else its session id.
fn tab_name(conn: &Connection, link: &Link, listing: Option<&Listing>) -> Result<(String, bool)> {
    type Named = (Option<String>, Option<String>, Option<String>);
    let (name, cwd, branch): Named = conn
        .prepare_cached("SELECT name, cwd, git_branch FROM agents WHERE id = ?1")?
        .query_row([link.orchestrator], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let key = effective_charge(conn, link.orchestrator)?
        .0
        .as_ref()
        .map(Charge::key);
    let unlinked = key.is_none();
    let name = key
        .or(name)
        .or_else(|| listing.and_then(|l| l.name.clone()))
        .unwrap_or_else(|| match (cwd, branch) {
            (Some(cwd), Some(branch)) => format!("{cwd} @ {branch}"),
            (Some(cwd), None) => cwd,
            (None, _) => link.session_id.clone(),
        });
    Ok((name, unlinked))
}

/// Every tab, live first, then by latest start.
pub fn workstreams(conn: &Connection) -> Result<Vec<Workstream>> {
    let listings = listings(conn)?;
    let tabs = tabs(conn)?;
    let tree = tree(conn, &tabs, &listings)?;
    let costs = chain_costs(conn, &tabs)?;
    let mut needs: HashMap<i64, i64> = HashMap::new();
    for row in waits(conn, &tabs)? {
        if let Some(w) = row.workstream {
            *needs.entry(w).or_default() += 1;
        }
    }
    let mut out = Vec::with_capacity(tabs.chains.len());
    for chain in &tabs.chains {
        let id = chain.id();
        let (link, live) = chain.drawn(&listings);
        let listing = listings.get(&link.session_id);
        let (name, fallback) = tab_name(conn, link, listing)?;
        let (cost_usd, unpriced) = costs.get(&id).copied().unwrap_or((None, 0));
        let needs_you = needs.get(&id).copied().unwrap_or(0);
        out.push(Workstream {
            id,
            name,
            fallback,
            cost_usd,
            unpriced,
            state: orchestrator_state(needs_you > 0, listing.and_then(|l| l.state.as_deref())),
            needs_you,
            session_id: Some(link.session_id.clone()),
            live,
            started_at: chain.started_at(),
            director: tree.nest.roots.get(&id).copied(),
        });
    }
    fold_nested(&mut out, &tree.nest);
    out.sort_by(|a, b| (b.live, &b.started_at, a.id).cmp(&(a.live, &a.started_at, b.id)));
    Ok(out)
}

/// $ per tab, by tab id; a tab with no request has no row.
pub fn workstream_costs(conn: &Connection) -> Result<Vec<WorkstreamCost>> {
    let tabs = tabs(conn)?;
    let mut out: Vec<WorkstreamCost> = chain_costs(conn, &tabs)?
        .into_iter()
        .map(|(workstream, (cost_usd, unpriced))| WorkstreamCost {
            workstream,
            cost_usd,
            unpriced,
        })
        .collect();
    out.sort_by_key(|c| c.workstream);
    Ok(out)
}

/// How far apart a registry wait and a hook wait can start and still be
/// one stop. The poller runs every second and stamps a poll before it
/// runs; the real store's pairs were 23 to 375 ms apart. A registry wait
/// starts at the poll that first saw it waiting (`waiting_since_ms`).
const SAME_STOP_MS: i64 = 5_000;

/// Everything waiting on the user, oldest first: the `needs_you` view, on
/// latest links (#68). A registry wait is its process's latest link's (see
/// [`listings`]), and a hook wait is listed only on a latest link, so each
/// names that session and its own chain's tab; it counts toward that tab's
/// root ([`Workstream::director`], else the tab itself), so the UI can
/// name the waiting chain and go to the root.
///
/// One stop is one row (#84): a registry wait that started within
/// [`SAME_STOP_MS`] of a hook wait on its link (the orchestrator's or a
/// worker's: the registry describes the whole process) is that wait, and
/// the hook's row stays, since it names the tool. The view keeps both
/// signals, as recorded; every count of waits reads this list.
pub fn needs_you(conn: &Connection) -> Result<Vec<NeedsYou>> {
    waits(conn, &tabs(conn)?)
}

/// [`needs_you`]'s rows, each on its chain's own tab.
fn waits(conn: &Connection, tabs: &Tabs) -> Result<Vec<NeedsYou>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.source, n.session_id, n.agent_id, n.waiting_for, n.since_ms,
             CASE WHEN r.kind = 'background' AND r.state IS 'blocked' AND r.bg_id <> ''
                 THEN r.bg_id END
         FROM needs_you AS n
         LEFT JOIN registry AS r ON r.id = n.registry
         WHERE n.source = 'hook' OR NOT EXISTS (SELECT 1 FROM needs_you AS h
             WHERE h.source = 'hook' AND h.session_id = n.session_id
               AND abs(h.since_ms - n.since_ms) <= ?1)
         ORDER BY n.since_ms, n.session_id, n.agent_id",
    )?;
    stmt.query_map([SAME_STOP_MS], |r| {
        let session_id: Option<String> = r.get(1)?;
        Ok(NeedsYou {
            source: r.get(0)?,
            workstream: session_id
                .as_deref()
                .and_then(|s| tabs.of_session(s))
                .map(Chain::id),
            session_id,
            agent_id: r.get(2)?,
            waiting_for: r.get(3)?,
            since_ms: r.get(4)?,
            bg_id: r.get(5)?,
        })
    })?
    .collect()
}

/// A drawn agent and the edge to its dispatcher or team lead, if any.
type Row = (GraphAgent, Option<(i64, String)>);

/// The agents of `session_id`, orchestrator first. The orchestrator's name,
/// state and registry wait come from the session's listing ([`listings`]),
/// and workers' states read its liveness.
///
/// A worker reads done once it has finished, by the model's one rule
/// ([`FINISHED_SQL`], [`ModelAgent::finished`]: its signals, or its lead's
/// process gone), or while a teammate rests ([`rests`]) from its newest
/// idle notice in this session's own transcript, the lead's, matched by its
/// meta name, or idle hook. Resting is not finished: the teammate can be
/// woken.
fn session_agents(
    conn: &Connection,
    session_id: &str,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Row>> {
    let listing = listings.get(session_id).unwrap_or(&NOT_LISTED);
    let notices = idle_notices(conn, session_id)?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT a.id, a.agent_id, a.kind, a.name, a.agent_type,
             a.description, a.model, a.cost_usd, a.context_tokens, a.context_pct,
             a.hook_state,
             EXISTS (SELECT 1 FROM needs_you AS n WHERE n.agent = a.id AND n.source = 'hook'),
             {FINISHED_SQL},
             (SELECT p.parent FROM edges AS p
                 WHERE p.child = a.id AND p.kind IN ('dispatch', 'teammate')),
             (SELECT p.kind FROM edges AS p
                 WHERE p.child = a.id AND p.kind IN ('dispatch', 'teammate')),
             CASE WHEN a.hook_state = 'idle' THEN a.hook_at_ms END,
             CASE WHEN a.kind = 'teammate' OR a.hook_state = 'idle' THEN
                 (SELECT CAST(round(max(unixepoch(e.timestamp, 'subsec')) * 1000) AS INTEGER)
                  FROM events AS e
                  WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id
                    AND e.type IN ('user', 'assistant'))
             END,
             a.session_id, a.role, l.lane
         FROM agents AS a JOIN agent_lanes AS l ON l.agent = a.id
         WHERE a.session_id = ?1
         ORDER BY a.agent_id IS NOT NULL, a.id"
    ))?;
    let rows = stmt.query_map([session_id], |r| {
        let agent_id: Option<String> = r.get(1)?;
        let needs: bool = r.get(11)?;
        let (name, state) = match agent_id {
            None => (
                listing.name.clone().or(r.get(3)?),
                orchestrator_state(needs || listing.waiting, listing.state.as_deref()),
            ),
            Some(_) => {
                let name: Option<String> = r.get(3)?;
                let notice = match r.get_ref(2)?.as_str()? {
                    "teammate" => name.as_ref().and_then(|n| notices.get(n)).copied(),
                    _ => None,
                };
                let idle = notice.max(r.get(15)?);
                let done = r.get::<_, bool>(12)? || rests(idle, r.get(16)?);
                let hook: Option<String> = r.get(10)?;
                (
                    name,
                    worker_state(needs, hook.as_deref(), done, listing.live),
                )
            }
        };
        let edge: Option<(i64, String)> = r
            .get::<_, Option<i64>>(13)?
            .zip(r.get::<_, Option<String>>(14)?);
        Ok((
            GraphAgent {
                id: r.get(0)?,
                agent_id,
                kind: r.get(2)?,
                name,
                agent_type: r.get(4)?,
                description: r.get(5)?,
                model: r.get(6)?,
                state,
                cost_usd: r.get(7)?,
                context_tokens: r.get(8)?,
                context_pct: r.get(9)?,
                parent: None,
                session_id: r.get(17)?,
                role: r.get(18)?,
                lane: lane(&r.get::<_, String>(19)?),
                // Read for drawn agents only ([`live_graph`]).
                charge: None,
                charge_key: None,
                charge_source: ChargeSource::Unlinked,
            },
            edge,
        ))
    })?;
    rows.collect()
}

/// A spawned chain to draw: the chain, its drawn link, and the edge from
/// its spawner.
struct Unit<'a> {
    chain: &'a Chain,
    link: &'a Link,
    spawner: (i64, String),
}

/// Every live chain beneath `chain` in the spawn tree (its ancestry holds
/// `chain`), at any depth: each one's drawn link, in start order (#63: a
/// finished chain is not drawn). Its orchestrator's edge is the spawn's
/// [`graph_kind`].
fn units<'a>(
    tabs: &'a Tabs,
    tree: &Tree,
    chain: &Chain,
    listings: &HashMap<String, Listing>,
) -> Vec<Unit<'a>> {
    let mut out: Vec<Unit> = tabs
        .chains
        .iter()
        .filter(|u| {
            tree.nest
                .up
                .get(&u.id())
                .is_some_and(|up| up.contains(&chain.id()))
        })
        .filter_map(|u| {
            let (link, live) = u.drawn(listings);
            let (spawner, kind) = tree.spawners.get(&u.id())?;
            live.then(|| Unit {
                chain: u,
                link,
                spawner: (*spawner, graph_kind(kind).to_string()),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let at = |u: &Unit| (u.link.started_at.is_none(), u.link.started_at.clone());
        at(a)
            .cmp(&at(b))
            .then_with(|| a.link.session_id.cmp(&b.link.session_id))
    });
    out
}

/// A spawn's [`GraphEdge`] kind: a `message` or `launch` (#108: a `--bg`
/// session's orchestrator under its spawner) spawn is `directs`; any other
/// kind is its own.
fn graph_kind(kind: &str) -> &str {
    match kind {
        "message" | "launch" => "directs",
        kind => kind,
    }
}

/// Every agent's spawn parent (`spawn_parents`), child to parent.
fn spawn_parents(conn: &Connection) -> Result<HashMap<i64, i64>> {
    conn.prepare_cached("SELECT child, parent FROM spawn_parents")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// `session_id`'s agents as [`session_agents`] reads them. One that hangs
/// under the orchestrator of any link of its `/clear` chain (a teammate's
/// lead can be another link's) hangs under `session_id`'s.
fn chain_agents(
    conn: &Connection,
    session_id: &str,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Row>> {
    let mut rows = session_agents(conn, session_id, listings)?;
    let Some(head) = orchestrator(&rows) else {
        return Ok(rows);
    };
    let leads: Vec<i64> = conn
        .prepare_cached(
            "SELECT a.id FROM sessions AS me
             JOIN sessions AS s ON s.process_root = me.process_root
             JOIN agents AS a ON a.session_id = s.session_id AND a.agent_id IS NULL
             WHERE me.session_id = ?1",
        )?
        .query_map([session_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    for (_, edge) in &mut rows {
        if let Some((parent, _)) = edge.as_mut().filter(|(p, _)| leads.contains(p)) {
            *parent = head;
        }
    }
    Ok(rows)
}

/// `parent`, else its nearest ancestor by `parents` (child to parent), of
/// the `drawn` agents; `None` past the top or round a cycle.
fn drawn_ancestor(mut parent: i64, parents: &HashMap<i64, i64>, drawn: &[i64]) -> Option<i64> {
    for _ in 0..=parents.len() {
        if drawn.contains(&parent) {
            return Some(parent);
        }
        parent = *parents.get(&parent)?;
    }
    None
}

/// The orchestrator's id among `rows`.
fn orchestrator(rows: &[Row]) -> Option<i64> {
    rows.iter()
        .find(|(a, _)| a.agent_id.is_none())
        .map(|(a, _)| a.id)
}

/// The $ of every request of the sessions `links`, and the sum of their
/// run times (#65: the header covers the links the canvas draws).
fn session_totals(conn: &Connection, links: &[&Link]) -> Result<(Option<f64>, Option<i64>)> {
    fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT (SELECT sum(r.cost_usd) FROM requests AS r WHERE r.session_id = ?1),
             (SELECT run_ms FROM sessions WHERE session_id = ?1)",
    )?;
    let mut totals = (None, None);
    for link in links {
        let (usd, run): (Option<f64>, Option<i64>) =
            stmt.query_row([&link.session_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        totals = (add(totals.0, usd), add(totals.1, run));
    }
    Ok(totals)
}

/// The running tree beneath tab `workstream` (any link's orchestrator id
/// reads as its tab): the link it draws, plus every live chain beneath it
/// in the spawn tree at any depth, each chain's orchestrator under its
/// spawner and its subagents and teammates. No worker that reads done is
/// drawn. `None` when `workstream` is no tab's.
pub fn live_graph(conn: &Connection, workstream: i64) -> Result<Option<LiveGraph>> {
    let tabs = tabs(conn)?;
    let Some(&at) = tabs.by_orchestrator.get(&workstream) else {
        return Ok(None);
    };
    let chain = &tabs.chains[at];
    let listings = listings(conn)?;
    let tree = tree(conn, &tabs, &listings)?;
    let (shown, live) = chain.drawn(&listings);
    let units = units(&tabs, &tree, chain, &listings);
    let upto = chain
        .links
        .iter()
        .position(|l| l.session_id == shown.session_id)
        .unwrap_or(0);
    let links: Vec<&Link> = chain.links[..=upto].iter().collect();
    let (session_cost_usd, run_ms) = session_totals(conn, &links)?;
    let mut rows = chain_agents(conn, &shown.session_id, &listings)?;
    let head = orchestrator(&rows);
    for unit in &units {
        for (agent, edge) in chain_agents(conn, &unit.link.session_id, &listings)? {
            let edge = match agent.agent_id {
                None => Some(unit.spawner.clone()),
                Some(_) => edge,
            };
            rows.push((agent, edge));
        }
    }
    // The drawn rows' edges, over the model's spawn parents for the walk
    // up through agents that are not drawn.
    let mut parents = spawn_parents(conn)?;
    parents.extend(
        rows.iter()
            .filter_map(|(a, edge)| Some((a.id, edge.as_ref()?.0))),
    );
    // Each drawn chain's other links' orchestrators read as its drawn one,
    // so what one of them (or a worker of it) spawned hangs under the
    // chain's drawn orchestrator, not past it under the chain's spawner.
    let drawn_chains =
        std::iter::once((chain, shown)).chain(units.iter().map(|u| (u.chain, u.link)));
    for (chain, drawn) in drawn_chains {
        for link in chain
            .links
            .iter()
            .filter(|l| l.orchestrator != drawn.orchestrator)
        {
            parents.insert(link.orchestrator, drawn.orchestrator);
        }
    }
    // A finished worker is left to the Audit (#81).
    rows.retain(|(a, _)| a.agent_id.is_none() || a.state != AgentState::Done);
    let drawn_ids: Vec<i64> = rows.iter().map(|(a, _)| a.id).collect();
    let heads: HashMap<String, i64> = rows
        .iter()
        .filter(|(a, _)| a.agent_id.is_none())
        .map(|(a, _)| (a.session_id.clone(), a.id))
        .collect();
    let mut agents = Vec::with_capacity(rows.len());
    let mut edges = Vec::new();
    for (mut agent, edge) in rows {
        // A worker nothing links yet (no meta, no discovery pass) hangs
        // under its session's orchestrator, its nearest drawn ancestor
        // for sure: never edgeless (#109).
        let edge = edge.or_else(|| {
            let head = heads
                .get(&agent.session_id)
                .filter(|_| agent.agent_id.is_some())?;
            let kind = if agent.kind == "teammate" {
                "teammate"
            } else {
                "dispatch"
            };
            Some((*head, kind.to_string()))
        });
        // One whose parent is not drawn (a finished dispatcher, a lead
        // outside the graph, a spawner on an earlier link or in a chain
        // that is not live) hangs under its nearest drawn ancestor, else
        // the tab's orchestrator.
        let edge = edge.and_then(|(parent, kind)| {
            let parent = drawn_ancestor(parent, &parents, &drawn_ids).or(head)?;
            Some((parent, kind))
        });
        if let Some((parent, kind)) = edge {
            agent.parent = Some(parent);
            edges.push(GraphEdge {
                kind,
                parent,
                child: agent.id,
            });
        }
        let (charge, source) = effective_charge(conn, agent.id)?;
        agent.charge_key = charge.as_ref().map(Charge::key);
        agent.charge = charge;
        agent.charge_source = source;
        agents.push(agent);
    }
    Ok(Some(LiveGraph {
        live,
        session_id: shown.session_id.clone(),
        session_cost_usd,
        run_ms,
        agents,
        edges,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_states_put_hooks_before_the_transcript() {
        use AgentState::*;
        assert_eq!(worker_state(true, Some("done"), true, false), NeedsYou);
        assert_eq!(worker_state(false, Some("done"), false, true), Done);
        assert_eq!(worker_state(false, Some("running"), true, true), Done);
        assert_eq!(worker_state(false, Some("idle"), false, false), Done);
        // An idle hook is a turn end the caller folds into `done`
        // (#83): one it does not is behind a newer record.
        assert_eq!(worker_state(false, Some("idle"), false, true), Working);
        assert_eq!(worker_state(false, Some("running"), false, true), Working);
        assert_eq!(worker_state(false, None, false, true), Working);
    }

    #[test]
    fn a_cross_session_spawn_draws_as_directs() {
        assert_eq!(graph_kind("message"), "directs");
        assert_eq!(graph_kind("launch"), "directs", "#108's --bg launch edge");
        assert_eq!(graph_kind("dispatch"), "dispatch");
        assert_eq!(graph_kind("teammate"), "teammate");
    }

    #[test]
    fn a_teammate_rests_from_its_turn_end_until_its_next_record() {
        assert!(rests(Some(5), Some(4)));
        assert!(rests(Some(5), Some(5)), "at its last record");
        // A notice matched by name alone can be an earlier teammate's of
        // that name.
        assert!(!rests(Some(5), None), "no record of its own yet");
        assert!(
            !rests(Some(5), Some(6)),
            "woken after it, or named before it"
        );
        assert!(!rests(None, Some(6)));
        assert!(!rests(None, None));
    }

    /// A lead's user record as Claude Code writes it: each teammate-message
    /// in its text, the body after the opening tag.
    fn notice(from: &str, at: &str) -> String {
        let body = serde_json::json!({
            "type": "idle_notification", "from": from, "timestamp": at,
            "idleReason": "available", "result": "Lorem </teammate-message> ipsum."
        });
        format!(
            "<teammate-message teammate_id=\"{from}\" color=\"blue\">\n{body}\n</teammate-message>"
        )
    }

    #[test]
    fn idle_notices_are_read_from_a_leads_record_text() {
        use serde_json::json;
        let ms = |s| crate::time::parse_timestamp(s).expect("stamp");
        let (a, b) = ("2026-10-09T10:04:00.020Z", "2026-10-09T10:05:00.020Z");
        let text = format!(
            "Lorem:\n{}\n{}\n<teammate-message teammate_id=\"m3\" summary=\"a > b\">\n\
             {{\"type\":\"idle_notification\",\"from\":\"m3\",\"timestamp\":\"{b}\"}}\n\
             </teammate-message><teammate-message teammate_id=\"m4\">Lorem.</teammate-message>\
             <teammate-message teammate_id=\"m5\">\n{{\"type\":\"idle_notification\",\
             \"from\":\"m5\"}}\n</teammate-message><teammate-message teammate_id=\"m6\">Lorem:\n\
             {}\n</teammate-message><teammate-message",
            notice("m1", a),
            notice("m2", b),
            notice("m7", b)
        );
        let record = json!({"type": "user", "message": {"role": "user", "content": text}});
        assert_eq!(
            notices_in(&record),
            [
                ("m1".to_string(), ms(a)),
                ("m2".to_string(), ms(b)),
                ("m3".to_string(), ms(b))
            ],
            "a quoted > in a tag, a plain message, no timestamp, a notice quoted in a \
             plain message, an open tag"
        );
        let blocks = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "content": notice("m9", a)},
            {"type": "text", "text": notice("m1", a)},
        ]}});
        assert_eq!(notices_in(&blocks), [("m1".to_string(), ms(a))]);
        let numbered = format!("<teammate-message teammate_id=\"m8\">1 {}", notice("m9", a));
        let numbered = json!({"type": "user", "message": {"content": numbered}});
        assert_eq!(
            notices_in(&numbered),
            [],
            "a plain message that opens with a number"
        );
        let open =
            json!({"type": "user", "message": {"content": "<teammate-message id=\"x\">Lorem"}});
        assert_eq!(notices_in(&open), [], "a plain message never closed");
        assert_eq!(notices_in(&json!({"type": "user"})), []);
    }

    /// [`live_graph`] reads each drawn session's notices: only the lead's
    /// own user records that can hold one, by `events_idle_notices`.
    #[test]
    fn idle_notices_are_read_by_their_partial_index() {
        let dir = crate::tail::test_dir("idle-notices-plan");
        let db = dir.join("strate.db");
        drop(super::super::Store::open(&db).expect("open"));
        let conn = super::super::Store::open_reader(&db).expect("reader");
        assert_eq!(
            super::super::query_plan(&conn, IDLE_NOTICES_SQL),
            ["SEARCH events USING INDEX events_idle_notices (session_id=?)"]
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn unknown_live_registry_states_are_idle() {
        assert_eq!(orchestrator_state(false, Some("lorem")), AgentState::Idle);
        assert_eq!(orchestrator_state(false, Some("gone")), AgentState::Done);
    }

    #[test]
    fn a_tab_nests_under_the_topmost_live_tab_of_its_spawn_ancestry() {
        let roots = |parents: &[(i64, i64)], live: &[i64]| {
            let parents: HashMap<i64, i64> = parents.iter().copied().collect();
            let mut v: Vec<_> = nest(&parents, |t| live.contains(&t))
                .roots
                .into_iter()
                .collect();
            v.sort();
            v
        };
        // 4 -> 3 -> 2 -> 1 (child -> spawner).
        let line = [(2, 1), (3, 2), (4, 3)];
        assert_eq!(roots(&line, &[1, 2, 3, 4]), [(2, 1), (3, 1), (4, 1)]);
        assert_eq!(
            roots(&line, &[1, 3]),
            [(2, 1), (3, 1), (4, 1)],
            "past an ancestor that is not live, and whether or not it is live itself"
        );
        assert_eq!(roots(&line, &[2, 4]), [(3, 2), (4, 2)], "1 is not live");
        assert_eq!(roots(&line, &[4]), [], "no live ancestor: its own tab");
        // Siblings under one root.
        assert_eq!(roots(&[(2, 1), (3, 1)], &[1, 2, 3]), [(2, 1), (3, 1)]);
        // A tab in a cycle nests nowhere, so no tab hides another; one
        // spawned from the cycle nests under its topmost live tab, the last
        // one walked before the walk would repeat.
        assert_eq!(roots(&[(1, 2), (2, 1)], &[1, 2]), []);
        assert_eq!(
            roots(&[(1, 2), (2, 3), (3, 1), (4, 3)], &[1, 2, 3, 4]),
            [(4, 2)]
        );
        let up = nest(&HashMap::from(line), |_| true).up;
        assert_eq!(up[&4], [3, 2, 1], "the spawn ancestry, nearest first");
        assert!(!up.contains_key(&1));
    }

    /// `workstreams()` sums every request on each call, so its cost lookup
    /// reads events from `events_requests` alone: one pass over the index,
    /// one search per request for a fuller copy, and no row read (#79).
    #[test]
    fn a_tabs_cost_is_read_from_the_request_index() {
        let dir = crate::tail::test_dir("tab-costs-plan");
        let db = dir.join("strate.db");
        drop(super::super::Store::open(&db).expect("open"));
        let conn = super::super::Store::open_reader(&db).expect("reader");
        let plan = super::super::query_plan(&conn, CHAIN_COSTS_SQL);
        let events: Vec<&str> = plan
            .iter()
            .map(String::as_str)
            .filter(|step| matches!(step.split(' ').nth(1), Some("e" | "o")))
            .collect();
        assert_eq!(
            events,
            [
                "SCAN e USING COVERING INDEX events_requests",
                "SEARCH o USING COVERING INDEX events_requests (request_key=?)",
            ],
            "{plan:?}"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    /// A store at `name` with `sql` run on it.
    fn seeded(name: &str, sql: &str) -> (std::path::PathBuf, super::super::Store) {
        let dir = crate::tail::test_dir(name);
        let store = super::super::Store::open_in(dir.join("strate.db"), "UTC").expect("open");
        store.conn.execute_batch(sql).expect("seed");
        (dir, store)
    }

    /// One orchestrator record per session of `sessions`: what makes a
    /// session a link with a tab.
    fn records(sessions: &[&str]) -> String {
        sessions
            .iter()
            .map(|s| {
                format!(
                    "INSERT INTO events (session_id, agent_id, file, byte_offset, raw)
                     VALUES ('{s}', NULL, '{s}', 0, '{{}}');"
                )
            })
            .collect()
    }

    /// `p1` then `p2` are one process (root p1); `q` is its own. Each
    /// request record is (offset, output tokens, $): `k3` streamed twice,
    /// `k4` is unpriced, and `k8` is a subagent-only stub session's, which
    /// has no tab.
    const CHAIN_COSTS: &str = "
        INSERT INTO sessions (session_id, process_root, started_at) VALUES
            ('p1', 'p1', '2026-10-07T10:00:00.000Z'), ('p2', 'p1', '2026-10-07T11:00:00.000Z'),
            ('q', NULL, '2026-10-07T12:00:00.000Z'), ('stub', NULL, NULL);
        INSERT INTO agents (id, session_id, agent_id, kind) VALUES
            (1, 'p1', NULL, 'orchestrator'), (2, 'p2', NULL, 'orchestrator'),
            (3, 'p2', 'sub', 'subagent'), (4, 'q', NULL, 'orchestrator'),
            (5, 'stub', NULL, 'orchestrator'), (6, 'stub', 'w', 'subagent');
        INSERT INTO events (session_id, agent_id, file, byte_offset, raw, request_key, model,
            usage, output_tokens, cost_usd) VALUES
            ('p1', NULL, 'p1', 50, '{}', 'k1', 'm', '{}', 1, 1.0),
            ('p2', NULL, 'p2', 100, '{}', 'k2', 'm', '{}', 1, 2.0),
            ('p2', NULL, 'p2', 200, '{}', 'k3', 'm', '{}', 5, 4.0),
            ('p2', NULL, 'p2', 250, '{}', 'k3', 'm', '{}', 9, 8.0),
            ('p2', NULL, 'p2', 600, '{}', 'k4', 'm', '{}', 1, NULL),
            ('p2', 'sub', 'p2-sub', 0, '{}', 'k5', 'm', '{}', 1, 16.0),
            ('q', NULL, 'q', 10, '{}', 'k7', 'm', '{}', 1, 64.0),
            ('stub', 'w', 'stub-w', 0, '{}', 'k8', 'm', '{}', 1, 128.0);";

    #[test]
    fn a_chains_tab_costs_every_request_of_its_links() {
        let (dir, store) = seeded("views-chain-costs", CHAIN_COSTS);
        let costs: Vec<(i64, Option<f64>, i64)> = workstream_costs(&store.conn)
            .expect("costs")
            .into_iter()
            .map(|c| (c.workstream, c.cost_usd, c.unpriced))
            .collect();
        // The p1 chain's tab is p1's orchestrator: k1, k2, k3's fuller
        // record and sub's k5; q's is k7.
        assert_eq!(costs, [(1, Some(27.0), 1), (4, Some(64.0), 0)]);
        let mut tabs: Vec<(i64, Option<f64>, i64)> = workstreams(&store.conn)
            .expect("workstreams")
            .into_iter()
            .map(|w| (w.id, w.cost_usd, w.unpriced))
            .collect();
        tabs.sort_by_key(|t| t.0);
        assert_eq!(tabs, costs, "the stub has no tab");
        assert_eq!(live_graph(&store.conn, 5).expect("query"), None);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_tab_is_named_by_its_drawn_links_charge_else_its_name_else_where_it_ran() {
        // c's own charge; d takes its spawner c's; n is named only; w
        // only ran somewhere; x has nothing.
        let sql = format!(
            "             INSERT INTO charges (id, key, kind, repo, number) VALUES
                 (7, 'issue_demo-5', 'issue', 'demo', 5);
             INSERT INTO sessions (session_id, started_at) VALUES
                 ('c', '2026-10-07T10:00:00.000Z'), ('d', '2026-10-07T11:00:00.000Z'),
                 ('n', '2026-10-07T12:00:00.000Z'), ('w', '2026-10-07T13:00:00.000Z'),
                 ('x', '2026-10-07T14:00:00.000Z');
             INSERT INTO agents (id, session_id, kind, name, cwd, git_branch, own_charge,
                     own_charge_source) VALUES
                 (1, 'c', 'orchestrator', 'impl_issue_demo-5', NULL, NULL, 7, 'name'),
                 (2, 'd', 'orchestrator', NULL, NULL, NULL, NULL, NULL),
                 (3, 'n', 'orchestrator', 'lorem ipsum', '/work/x', 'main', NULL, NULL),
                 (4, 'w', 'orchestrator', NULL, '/work/x', 'feat/y', NULL, NULL),
                 (5, 'x', 'orchestrator', NULL, NULL, NULL, NULL, NULL);
             INSERT INTO edges (kind, parent, child) VALUES ('message', 1, 2);
             {}",
            records(&["c", "d", "n", "w", "x"])
        );
        let (dir, store) = seeded("views-tab-names", &sql);
        let mut names: Vec<(i64, String, bool)> = workstreams(&store.conn)
            .expect("workstreams")
            .into_iter()
            .map(|w| (w.id, w.name, w.fallback))
            .collect();
        names.sort();
        let named = |id, name: &str, fallback| (id, name.to_string(), fallback);
        assert_eq!(
            names,
            [
                named(1, "issue_demo-5", false),
                named(2, "issue_demo-5", false),
                named(3, "lorem ipsum", true),
                named(4, "/work/x @ feat/y", true),
                named(5, "x", true),
            ]
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_session_spawned_from_its_own_chain_is_no_unit_and_nests_nowhere() {
        // p is the live, drawn link; c, an earlier link of p's chain, was
        // spawned by p's orchestrator.
        let sql = format!(
            "             INSERT INTO sessions (session_id, process_root, started_at) VALUES
                 ('c', 'c', '2026-10-07T09:00:00.000Z'), ('p', 'c', '2026-10-07T10:00:00.000Z');
             INSERT INTO agents (id, session_id, kind) VALUES (1, 'c', 'orchestrator'),
                 (2, 'p', 'orchestrator');
             INSERT INTO edges (kind, parent, child) VALUES ('message', 2, 1);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'c', '/work/x', 1, 'busy', 1, 1);
             {}",
            records(&["c", "p"])
        );
        let (dir, store) = seeded("views-own-unit", &sql);
        let rows = workstreams(&store.conn).expect("workstreams");
        assert_eq!(rows.len(), 1, "one chain, one tab: {rows:?}");
        assert_eq!(
            (rows[0].id, rows[0].session_id.as_deref(), rows[0].director),
            (1, Some("p"), None)
        );
        let graph = live_graph(&store.conn, 2).expect("query").expect("graph");
        assert_eq!(
            graph.session_id, "p",
            "any link's orchestrator reads as its tab"
        );
        assert_eq!(graph.agents.len(), 1, "{:?}", graph.agents);
        assert!(graph.edges.is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_teammate_led_from_another_link_hangs_under_the_drawn_orchestrator() {
        // p1 then p2: one live process (root p1). The team lead is p1's
        // orchestrator; one teammate's transcript is p2's, the other's
        // p1's, so it reads done and is not drawn.
        let sql = format!(
            "             INSERT INTO sessions (session_id, process_root, started_at) VALUES
                 ('p1', 'p1', '2026-10-07T10:00:00.000Z'),
                 ('p2', 'p1', '2026-10-07T11:00:00.000Z');
             INSERT INTO agents (id, session_id, agent_id, kind) VALUES
                 (1, 'p1', NULL, 'orchestrator'), (2, 'p2', NULL, 'orchestrator'),
                 (3, 'p2', 'm1', 'teammate'), (4, 'p1', 'm2', 'teammate');
             INSERT INTO edges (kind, parent, child) VALUES ('teammate', 1, 3), ('teammate', 1, 4);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'p2', '/work/x', 1, 'busy', 1, 1);
             {}",
            records(&["p1", "p2"])
        );
        let (dir, store) = seeded("views-chain-teammates", &sql);
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        assert_eq!(graph.session_id, "p2");
        let drawn: Vec<(i64, Option<i64>)> =
            graph.agents.iter().map(|a| (a.id, a.parent)).collect();
        assert_eq!(drawn, [(2, None), (3, Some(2))]);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn the_graph_draws_no_finished_worker_on_any_link() {
        // p1 then p2: one live process (root p1), whose p2 spawned the live
        // unit u. Each of p2 and u has a running and a finished worker;
        // p1's teammate is an earlier link's.
        let sql = format!(
            "             INSERT INTO sessions (session_id, process_root, started_at) VALUES
                 ('p1', 'p1', '2026-10-07T10:00:00.000Z'),
                 ('p2', 'p1', '2026-10-07T11:00:00.000Z'),
                 ('u', 'u', '2026-10-07T10:30:00.000Z');
             INSERT INTO agents (id, session_id, agent_id, kind, hook_state) VALUES
                 (1, 'p1', NULL, 'orchestrator', NULL),
                 (2, 'p2', NULL, 'orchestrator', NULL),
                 (3, 'p2', 'w1', 'subagent', NULL), (4, 'p2', 'w2', 'subagent', 'done'),
                 (5, 'p1', 'm1', 'teammate', NULL),
                 (6, 'u', NULL, 'orchestrator', NULL),
                 (7, 'u', 'w3', 'subagent', NULL), (8, 'u', 'w4', 'subagent', 'done');
             INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                 ('dispatch', 2, 3, 't1'), ('dispatch', 2, 4, 't2'), ('teammate', 1, 5, NULL),
                 ('dispatch', 6, 7, 't3'), ('dispatch', 6, 8, 't4'), ('message', 2, 6, NULL);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms) VALUES
                 ('pid:1:1', 'interactive', 'p2', '/work/x', 1, 'busy', 1, 1),
                 ('pid:2:1', 'interactive', 'u', '/work/x', 1, 'busy', 1, 1);
             {}",
            records(&["p1", "p2", "u"])
        );
        let (dir, store) = seeded("views-no-done-workers", &sql);
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        let drawn: Vec<(i64, Option<i64>)> =
            graph.agents.iter().map(|a| (a.id, a.parent)).collect();
        assert_eq!(drawn, [(2, None), (3, Some(2)), (6, Some(2)), (7, Some(6))]);
        let edges: Vec<(&str, i64, i64)> = graph
            .edges
            .iter()
            .map(|e| (e.kind.as_str(), e.parent, e.child))
            .collect();
        assert_eq!(
            edges,
            [("dispatch", 2, 3), ("directs", 2, 6), ("dispatch", 6, 7)]
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_unit_hangs_under_its_running_spawner_else_the_drawn_orchestrator() {
        // p's running subagent w1 spawned u; its finished subagent w2
        // spawned v; p1, an earlier link of p's chain, spawned x.
        let sql = format!(
            "             INSERT INTO sessions (session_id, process_root, started_at) VALUES
                 ('p1', 'p1', '2026-10-07T09:00:00.000Z'),
                 ('p', 'p1', '2026-10-07T10:00:00.000Z'),
                 ('u', 'u', '2026-10-07T10:10:00.000Z'), ('v', 'v', '2026-10-07T10:20:00.000Z'),
                 ('x', 'x', '2026-10-07T10:30:00.000Z');
             INSERT INTO agents (id, session_id, agent_id, kind, hook_state) VALUES
                 (1, 'p', NULL, 'orchestrator', NULL), (2, 'p', 'w1', 'subagent', NULL),
                 (3, 'p', 'w2', 'subagent', 'done'), (4, 'u', NULL, 'orchestrator', NULL),
                 (5, 'v', NULL, 'orchestrator', NULL), (6, 'x', NULL, 'orchestrator', NULL),
                 (7, 'p1', NULL, 'orchestrator', NULL);
             INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                 ('dispatch', 1, 2, 't1'), ('dispatch', 1, 3, 't2'),
                 ('message', 2, 4, NULL), ('message', 3, 5, NULL), ('message', 7, 6, NULL);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms) VALUES
                 ('p', 'interactive', 'p1', '/w', 1, 'busy', 1, 1),
                 ('u', 'background', 'u', '/w', 1, 'busy', 1, 1),
                 ('v', 'background', 'v', '/w', 1, 'busy', 1, 1),
                 ('x', 'background', 'x', '/w', 1, 'busy', 1, 1);
             {}",
            records(&["p1", "p", "u", "v", "x"])
        );
        let (dir, store) = seeded("views-unit-spawners", &sql);
        let graph = live_graph(&store.conn, 7).expect("query").expect("graph");
        assert_eq!(graph.session_id, "p");
        let edges: Vec<(&str, i64, i64)> = graph
            .edges
            .iter()
            .map(|e| (e.kind.as_str(), e.parent, e.child))
            .collect();
        assert_eq!(
            edges,
            [
                ("dispatch", 1, 2),
                ("directs", 2, 4),
                ("directs", 1, 5),
                ("directs", 1, 6)
            ]
        );
        let directors: Vec<(i64, Option<i64>)> = workstreams(&store.conn)
            .expect("workstreams")
            .into_iter()
            .map(|w| (w.id, w.director))
            .collect();
        for unit in [4, 5, 6] {
            assert!(directors.contains(&(unit, Some(7))), "{directors:?}");
        }
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    /// A company tree (#109): t is the live root tab. Its subagent 2
    /// spawned b, whose subagent 4 spawned c; t also spawned e, whose
    /// process is gone, and e spawned f, whose subagent 8 waits on a tool.
    const TREE: &str = "
        INSERT INTO sessions (session_id, process_root, started_at) VALUES
            ('t', 't', '2026-10-07T10:00:00.000Z'), ('b', 'b', '2026-10-07T10:10:00.000Z'),
            ('c', 'c', '2026-10-07T10:20:00.000Z'), ('e', 'e', '2026-10-07T10:30:00.000Z'),
            ('f', 'f', '2026-10-07T10:40:00.000Z');
        INSERT INTO charges (id, key, kind, repo, number) VALUES
            (7, 'issue_demo-5', 'issue', 'demo', 5), (8, 'issue_demo-6', 'issue', 'demo', 6);
        INSERT INTO agents (id, session_id, agent_id, kind, role, own_charge, own_charge_source,
                hook_state, hook_waiting_for, hook_at_ms) VALUES
            (1, 't', NULL, 'orchestrator', 'director', 7, 'name', NULL, NULL, NULL),
            (2, 't', 'w1', 'subagent', 'impl', NULL, NULL, NULL, NULL, NULL),
            (3, 'b', NULL, 'orchestrator', NULL, NULL, NULL, NULL, NULL, NULL),
            (4, 'b', 'w2', 'subagent', NULL, NULL, NULL, NULL, NULL, NULL),
            (5, 'c', NULL, 'orchestrator', NULL, 8, 'branch', NULL, NULL, NULL),
            (6, 'e', NULL, 'orchestrator', NULL, NULL, NULL, NULL, NULL, NULL),
            (7, 'f', NULL, 'orchestrator', NULL, NULL, NULL, NULL, NULL, NULL),
            (8, 'f', 'w3', 'subagent', NULL, NULL, NULL, 'needs_you', 'Bash', 10000),
            (9, 'f', 'w4', 'subagent', NULL, NULL, NULL, 'done', NULL, NULL);
        INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
            ('dispatch', 1, 2, 't1'), ('message', 2, 3, NULL), ('dispatch', 3, 4, 't2'),
            ('message', 4, 5, NULL), ('message', 1, 6, NULL), ('message', 6, 7, NULL),
            ('dispatch', 7, 8, 't3'), ('dispatch', 7, 9, 't4');
        INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, state,
            first_seen_ms, last_seen_ms, gone) VALUES
            ('t', 'interactive', 't', '/work/x', 1, 'busy', NULL, 1, 1, 0),
            ('b', 'background', 'b', '/work/x', 1, NULL, 'running', 1, 1, 0),
            ('c', 'background', 'c', '/work/x', 1, NULL, 'running', 1, 1, 0),
            ('e', 'background', 'e', '/work/x', 1, NULL, 'done', 1, 1, 1),
            ('f', 'background', 'f', '/work/x', 1, NULL, 'running', 1, 1, 0);";

    #[test]
    fn a_live_tree_nests_under_its_root_tab_at_any_depth() {
        let sql = format!("{TREE}{}", records(&["t", "b", "c", "e", "f"]));
        let (dir, store) = seeded("views-live-tree", &sql);
        let mut tabs: Vec<(i64, bool, Option<i64>, i64, AgentState)> = workstreams(&store.conn)
            .expect("workstreams")
            .into_iter()
            .map(|w| (w.id, w.live, w.director, w.needs_you, w.state))
            .collect();
        tabs.sort_by_key(|t| t.0);
        use AgentState::*;
        assert_eq!(
            tabs,
            [
                (1, true, None, 1, NeedsYou),
                (3, true, Some(1), 0, Working),
                (5, true, Some(1), 0, Working),
                // e is gone, so no root, but f is nested under it.
                (6, false, Some(1), 1, NeedsYou),
                (7, true, Some(1), 1, NeedsYou),
            ],
            "c's spawner b is nested, and f's spawner e is not live: both nest under t"
        );
        let waits: Vec<(Option<String>, Option<i64>)> = needs_you(&store.conn)
            .expect("needs_you")
            .into_iter()
            .map(|n| (n.agent_id, n.workstream))
            .collect();
        assert_eq!(
            waits,
            [(Some("w3".to_string()), Some(7))],
            "its own chain f's tab, whose director (above) is the root t"
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_root_tabs_graph_draws_its_whole_running_tree() {
        let sql = format!("{TREE}{}", records(&["t", "b", "c", "e", "f"]));
        let (dir, store) = seeded("views-live-tree-graph", &sql);
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        let drawn: Vec<(i64, Option<i64>)> =
            graph.agents.iter().map(|a| (a.id, a.parent)).collect();
        assert_eq!(
            drawn,
            [
                (1, None),
                (2, Some(1)),
                (3, Some(2)),
                (4, Some(3)),
                (5, Some(4)),
                (7, Some(1)),
                (8, Some(7)),
            ],
            "e is not drawn, so f hangs under e's spawner; f's finished w4 is not drawn"
        );
        let edges: Vec<(&str, i64, i64)> = graph
            .edges
            .iter()
            .map(|e| (e.kind.as_str(), e.parent, e.child))
            .collect();
        assert_eq!(
            edges,
            [
                ("dispatch", 1, 2),
                ("directs", 2, 3),
                ("dispatch", 3, 4),
                ("directs", 4, 5),
                ("directs", 1, 7),
                ("dispatch", 7, 8),
            ]
        );
        // A nested tab draws its own subtree.
        let graph = live_graph(&store.conn, 3).expect("query").expect("graph");
        let ids: Vec<i64> = graph.agents.iter().map(|a| a.id).collect();
        assert_eq!(ids, [3, 4, 5]);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_unit_spawned_from_an_earlier_link_hangs_under_its_chains_drawn_orchestrator() {
        // t1 then t2 (t's chain, t2 drawn); t2's worker 4 spawned b, whose
        // chain is b1 then b2 (b2 drawn). b1's orchestrator spawned c and
        // b1's worker 6 spawned g; t1's worker 2 spawned h. No earlier
        // link's agent is drawn.
        let sql = format!(
            "             INSERT INTO sessions (session_id, process_root, started_at) VALUES
                 ('t1', 't1', '2026-10-07T10:00:00.000Z'), ('t2', 't1', '2026-10-07T11:00:00.000Z'),
                 ('b1', 'b1', '2026-10-07T10:10:00.000Z'), ('b2', 'b1', '2026-10-07T10:50:00.000Z'),
                 ('c', 'c', '2026-10-07T10:20:00.000Z'), ('g', 'g', '2026-10-07T10:25:00.000Z'),
                 ('h', 'h', '2026-10-07T10:30:00.000Z');
             INSERT INTO agents (id, session_id, agent_id, kind) VALUES
                 (1, 't1', NULL, 'orchestrator'), (2, 't1', 'w1', 'subagent'),
                 (3, 't2', NULL, 'orchestrator'), (4, 't2', 'w2', 'subagent'),
                 (5, 'b1', NULL, 'orchestrator'), (6, 'b1', 'w3', 'subagent'),
                 (7, 'b2', NULL, 'orchestrator'), (8, 'c', NULL, 'orchestrator'),
                 (9, 'g', NULL, 'orchestrator'), (10, 'h', NULL, 'orchestrator');
             INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                 ('dispatch', 1, 2, 't1'), ('dispatch', 3, 4, 't2'), ('message', 4, 5, NULL),
                 ('dispatch', 5, 6, 't3'), ('message', 5, 8, NULL), ('message', 6, 9, NULL),
                 ('message', 2, 10, NULL);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, state,
                 first_seen_ms, last_seen_ms, gone) VALUES
                 ('t', 'interactive', 't2', '/work/x', 1, 'busy', NULL, 1, 1, 0),
                 ('b', 'background', 'b2', '/work/x', 1, NULL, 'running', 1, 1, 0),
                 ('c', 'background', 'c', '/work/x', 1, NULL, 'running', 1, 1, 0),
                 ('g', 'background', 'g', '/work/x', 1, NULL, 'running', 1, 1, 0),
                 ('h', 'background', 'h', '/work/x', 1, NULL, 'running', 1, 1, 0);
             {}",
            records(&["t1", "t2", "b1", "b2", "c", "g", "h"])
        );
        let (dir, store) = seeded("views-earlier-link-spawners", &sql);
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        assert_eq!(graph.session_id, "t2");
        let edges: Vec<(&str, i64, i64)> = graph
            .edges
            .iter()
            .map(|e| (e.kind.as_str(), e.parent, e.child))
            .collect();
        assert_eq!(
            edges,
            [
                ("dispatch", 3, 4),
                ("directs", 7, 8),
                ("directs", 7, 9),
                ("directs", 3, 10),
                ("directs", 4, 7),
            ],
            "c and g under b2's orchestrator, not b's spawner; h under t2's"
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_drawn_agent_carries_its_models_session_role_lane_and_charge() {
        let sql = format!("{TREE}{}", records(&["t", "b", "c", "e", "f"]));
        let (dir, store) = seeded("views-live-tree-model", &sql);
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        let model: HashMap<i64, ModelAgent> = agent_model(&store.conn)
            .expect("model")
            .into_iter()
            .map(|a| (a.id, a))
            .collect();
        for agent in &graph.agents {
            let m = &model[&agent.id];
            assert_eq!(
                (
                    &agent.session_id,
                    &agent.role,
                    agent.lane,
                    &agent.charge,
                    agent.charge_source
                ),
                (&m.session_id, &m.role, m.lane, &m.charge, m.charge_source),
                "agent {}",
                agent.id
            );
            assert_eq!(agent.charge_key, m.charge.as_ref().map(Charge::key));
        }
        let by_id = |id: i64| graph.agents.iter().find(|a| a.id == id).expect("drawn");
        let t = by_id(1);
        assert_eq!(
            (t.session_id.as_str(), t.role.as_deref(), t.lane),
            ("t", Some("director"), Lane::Interactive)
        );
        assert_eq!(
            (t.charge_key.as_deref(), t.charge_source),
            (Some("issue_demo-5"), ChargeSource::Name)
        );
        let w = by_id(8);
        assert_eq!(
            (w.lane, w.charge_key.as_deref(), w.charge_source),
            (Lane::Subagent, Some("issue_demo-5"), ChargeSource::Parent),
            "through e, which is not drawn"
        );
        let c = by_id(5);
        assert_eq!(
            (c.lane, c.charge_key.as_deref(), c.charge_source),
            (
                Lane::BackgroundSession,
                Some("issue_demo-6"),
                ChargeSource::Branch
            )
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_running_worker_under_a_hidden_parent_hangs_under_its_nearest_drawn_ancestor() {
        // p is live: its subagent 2 finished, but 2's own dispatch 3 runs,
        // and so does 4, a teammate whose lead is q's undrawn orchestrator.
        let sql = format!(
            "             INSERT INTO sessions (session_id, started_at) VALUES
                 ('p', '2026-10-07T10:00:00.000Z'), ('q', '2026-10-07T09:00:00.000Z');
             INSERT INTO agents (id, session_id, agent_id, kind, hook_state) VALUES
                 (1, 'p', NULL, 'orchestrator', NULL), (2, 'p', 'w1', 'subagent', 'done'),
                 (3, 'p', 'w2', 'subagent', NULL), (4, 'p', 'm1', 'teammate', NULL),
                 (5, 'q', NULL, 'orchestrator', NULL);
             INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                 ('dispatch', 1, 2, 't1'), ('dispatch', 2, 3, 't2'), ('teammate', 5, 4, NULL);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'p', '/work/x', 1, 'busy', 1, 1);
             {}",
            records(&["p", "q"])
        );
        let (dir, store) = seeded("views-hidden-parent", &sql);
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        let drawn: Vec<(i64, Option<i64>)> =
            graph.agents.iter().map(|a| (a.id, a.parent)).collect();
        assert_eq!(drawn, [(1, None), (3, Some(1)), (4, Some(1))]);
        let edges: Vec<(&str, i64, i64)> = graph
            .edges
            .iter()
            .map(|e| (e.kind.as_str(), e.parent, e.child))
            .collect();
        assert_eq!(edges, [("dispatch", 1, 3), ("teammate", 1, 4)]);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_registry_wait_and_a_hook_wait_on_one_stop_are_one_row() {
        // Each session has a hook wait from 10000 and a registry wait whose
        // start differs: s by 375 ms (the real store's gap), u by 5000 ms,
        // v by 5001 ms, x on a subagent by 2000 ms, b blocked in the
        // background. g's registry row is gone, so g needs nothing. A
        // wait's start is never its row's last change (99999). Only s and
        // v have orchestrator records, and so tabs.
        let sql = format!(
            "             INSERT INTO sessions (session_id) VALUES ('s'), ('u'), ('v'), ('x'), ('b'),
                 ('g');
             INSERT INTO agents (session_id, agent_id, kind, hook_state, hook_waiting_for,
                 hook_at_ms)
             SELECT session_id, NULL, 'orchestrator', 'needs_you', 'AskUserQuestion', 10000
             FROM sessions WHERE session_id <> 'x'
             UNION ALL SELECT 'x', NULL, 'orchestrator', NULL, NULL, NULL
             UNION ALL SELECT 'x', 'a1', 'subagent', 'needs_you', 'Bash', 10000;
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, state,
                 waiting_for, first_seen_ms, last_seen_ms, gone, waiting_since_ms) VALUES
                 ('s', 'interactive', 's', '/w', 1, 'waiting', NULL, 'input needed', 1, 99999, 0,
                     10375),
                 ('u', 'interactive', 'u', '/w', 1, 'waiting', NULL, 'input needed', 1, 99999, 0,
                     15000),
                 ('v', 'interactive', 'v', '/w', 1, 'waiting', NULL, 'input needed', 1, 99999, 0,
                     15001),
                 ('x', 'interactive', 'x', '/w', 1, 'waiting', NULL, 'input needed', 1, 99999, 0,
                     12000),
                 ('b', 'background', 'b', '/w', 1, NULL, 'blocked', NULL, 1, 99999, 0, 10200),
                 ('g', 'interactive', 'g', '/w', 1, 'waiting', NULL, 'input needed', 1, 99999, 1,
                     10000);
             {}",
            records(&["s", "v"])
        );
        let (dir, store) = seeded("views-one-stop", &sql);
        let rows: Vec<_> = needs_you(&store.conn)
            .expect("needs_you")
            .into_iter()
            .map(|n| (n.source, n.session_id, n.agent_id, n.since_ms))
            .collect();
        let row = |source: &str, session: &str, agent: Option<&str>, since| {
            (
                source.into(),
                Some(session.into()),
                agent.map(String::from),
                Some(since),
            )
        };
        assert_eq!(
            rows,
            [
                row("hook", "b", None, 10000),
                row("hook", "s", None, 10000),
                row("hook", "u", None, 10000),
                row("hook", "v", None, 10000),
                row("hook", "x", Some("a1"), 10000),
                row("registry", "v", None, 15001),
            ]
        );
        // The header's and the tab's count read the same rows.
        let counts: Vec<(Option<String>, i64, AgentState)> = workstreams(&store.conn)
            .expect("workstreams")
            .into_iter()
            .map(|w| (w.session_id, w.needs_you, w.state))
            .collect();
        let tab = |s: &str, n| (Some(s.to_string()), n, AgentState::NeedsYou);
        assert_eq!(counts.len(), 2, "{counts:?}");
        assert!(counts.contains(&tab("s", 1)), "{counts:?}");
        assert!(counts.contains(&tab("v", 2)), "{counts:?}");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn only_a_parked_background_job_carries_its_registry_short_id() {
        // p: a background job blocked on the user. q: the same job with no
        // short id. i: an interactive wait. w: a background job waiting on a
        // named prompt, which is a live wait, not a parked one.
        let sql = "INSERT INTO sessions (session_id) VALUES ('p'), ('q'), ('i'), ('w');
             INSERT INTO registry (key, kind, session_id, bg_id, cwd, started_at_ms, status,
                 state, waiting_for, first_seen_ms, last_seen_ms, gone, waiting_since_ms) VALUES
                 ('p', 'background', 'p', 'abcd1234', '/w', 1, NULL, 'blocked', NULL, 1, 9, 0, 1),
                 ('q', 'background', 'q', '', '/w', 1, NULL, 'blocked', NULL, 1, 9, 0, 2),
                 ('i', 'interactive', 'i', NULL, '/w', 1, 'waiting', NULL, 'input', 1, 9, 0, 3),
                 ('w', 'background', 'w', 'ffff0000', '/w', 1, 'waiting', NULL, 'input', 1, 9, 0,
                     4);";
        let (dir, store) = seeded("views-parked-short-id", sql);
        let rows: Vec<_> = needs_you(&store.conn)
            .expect("needs_you")
            .into_iter()
            .map(|n| (n.session_id, n.bg_id))
            .collect();
        let row = |s: &str, id: Option<&str>| (Some(s.to_string()), id.map(String::from));
        assert_eq!(
            rows,
            [
                row("p", Some("abcd1234")),
                row("q", None),
                row("i", None),
                row("w", None),
            ]
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v17_registry_wait_starts_at_its_last_change() {
        let dir = crate::tail::test_dir("views-v17-wait-start");
        let db = dir.join("strate.db");
        let mut v17 = Connection::open(&db).expect("v17 db");
        super::super::migrate::migrate(&mut v17, &super::super::migrate::MIGRATIONS[..17])
            .expect("v17");
        v17.execute_batch(
            "INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, state,
                 waiting_for, first_seen_ms, last_seen_ms) VALUES
                 ('w', 'interactive', 'w', '/w', 1, 'waiting', NULL, 'input', 1, 500),
                 ('b', 'background', 'b', '/w', 1, NULL, 'blocked', NULL, 1, 600),
                 ('i', 'interactive', 'i', '/w', 1, 'busy', NULL, NULL, 1, 700);",
        )
        .expect("v17 rows");
        drop(v17);

        let store = super::super::Store::open_in(&db, "UTC").expect("upgrade");
        let starts: Vec<(String, Option<i64>)> = store
            .conn
            .prepare("SELECT key, waiting_since_ms FROM registry ORDER BY key")
            .expect("starts")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("starts");
        let want = [("b", Some(600)), ("i", None), ("w", Some(500))];
        assert_eq!(starts, want.map(|(k, s)| (k.to_string(), s)));
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn the_more_urgent_state_wins_a_fold() {
        use AgentState::*;
        let order = [Done, Idle, Working, NeedsYou];
        for (i, a) in order.iter().enumerate() {
            for (j, b) in order.iter().enumerate() {
                assert_eq!(more_urgent(*a, *b), order[i.max(j)], "{a:?} {b:?}");
            }
        }
    }

    fn keys(value: impl Serialize) -> Vec<String> {
        let value = serde_json::to_value(value).expect("json");
        value.as_object().expect("object").keys().cloned().collect()
    }

    /// The shapes `src/api.ts` declares.
    #[test]
    fn views_serialize_in_the_webview_shape() {
        let agent = GraphAgent {
            id: 1,
            agent_id: None,
            kind: "orchestrator".into(),
            name: None,
            agent_type: None,
            description: None,
            model: None,
            state: AgentState::NeedsYou,
            cost_usd: None,
            context_tokens: None,
            context_pct: None,
            parent: None,
            session_id: "s".into(),
            role: Some("impl".into()),
            lane: Lane::BackgroundSession,
            charge: Some(Charge {
                kind: crate::charge::ChargeKind::Issue,
                repo: Some("strate".into()),
                number: Some(109),
                topic: None,
            }),
            charge_key: Some("issue_strate-109".into()),
            charge_source: ChargeSource::Parent,
        };
        assert_eq!(
            serde_json::to_value(agent.state).expect("json"),
            "needs_you"
        );
        let mut got = keys(&agent);
        got.sort();
        assert_eq!(
            got,
            [
                "agentId",
                "agentType",
                "charge",
                "chargeKey",
                "chargeSource",
                "contextPct",
                "contextTokens",
                "costUsd",
                "description",
                "id",
                "kind",
                "lane",
                "model",
                "name",
                "parent",
                "role",
                "sessionId",
                "state"
            ]
        );
        let json = serde_json::to_value(&agent).expect("json");
        assert_eq!(
            (
                &json["lane"],
                &json["chargeSource"],
                &json["chargeKey"],
                &json["role"]
            ),
            (
                &serde_json::json!("background-session"),
                &serde_json::json!("parent"),
                &serde_json::json!("issue_strate-109"),
                &serde_json::json!("impl")
            )
        );
        assert_eq!(
            json["charge"],
            serde_json::json!({"kind": "issue", "repo": "strate", "number": 109, "topic": null})
        );
        let unlinked = GraphAgent {
            charge: None,
            charge_key: None,
            charge_source: ChargeSource::Unlinked,
            lane: Lane::Interactive,
            role: None,
            ..agent
        };
        let json = serde_json::to_value(&unlinked).expect("json");
        assert_eq!(
            (&json["charge"], &json["chargeKey"], &json["chargeSource"]),
            (
                &serde_json::Value::Null,
                &serde_json::Value::Null,
                &serde_json::json!("unlinked")
            )
        );
        assert_eq!(json["lane"], "interactive");
        assert_eq!(json["sessionId"], "s");
        let graph = LiveGraph {
            session_id: "s".into(),
            live: true,
            session_cost_usd: None,
            run_ms: None,
            agents: vec![],
            edges: vec![],
        };
        let mut got = keys(&graph);
        got.sort();
        assert_eq!(
            got,
            [
                "agents",
                "edges",
                "live",
                "runMs",
                "sessionCostUsd",
                "sessionId"
            ]
        );
        let mut got = keys(Workstream {
            id: 1,
            name: "n".into(),
            fallback: false,
            cost_usd: None,
            unpriced: 0,
            state: AgentState::Done,
            needs_you: 0,
            session_id: None,
            live: false,
            started_at: None,
            director: None,
        });
        got.sort();
        assert_eq!(
            got,
            [
                "costUsd",
                "director",
                "fallback",
                "id",
                "live",
                "name",
                "needsYou",
                "sessionId",
                "startedAt",
                "state",
                "unpriced"
            ]
        );
        let mut got = keys(NeedsYou {
            source: "hook".into(),
            session_id: None,
            agent_id: None,
            waiting_for: None,
            since_ms: None,
            workstream: None,
            bg_id: None,
        });
        got.sort();
        assert_eq!(
            got,
            [
                "agentId",
                "bgId",
                "sessionId",
                "sinceMs",
                "source",
                "waitingFor",
                "workstream"
            ]
        );
        let mut got = keys(WorkstreamCost {
            workstream: 1,
            cost_usd: None,
            unpriced: 0,
        });
        got.sort();
        assert_eq!(got, ["costUsd", "unpriced", "workstream"]);
        assert_eq!(
            keys(GraphEdge {
                kind: "dispatch".into(),
                parent: 1,
                child: 2
            }),
            ["child", "kind", "parent"]
        );
    }
}
