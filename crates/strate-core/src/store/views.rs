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
//! A tab ([`Workstream`]) is one `/clear` chain (#107, an interim shape
//! until #109): the sessions of one process root with orchestrator records,
//! its links. Its id is its first link's orchestrator's row id (any link's
//! orchestrator id reads as the tab in [`live_graph`]), and it draws one
//! link: a live one first, then the latest start. It is named by that
//! link's orchestrator: its effective charge's key (the `agent_charges`
//! rule), else its name, its registry name, its cwd and branch, its session
//! id; a tab with no charge is `fallback` (Unlinked). Its $ is every
//! request of its links, every agent's.
//!
//! Its live graph is the drawn link's orchestrator plus that session's
//! subagents and teammates that still run, with dispatch and teammate edges
//! among them. A worker that reads done is not drawn (#81): finished
//! workers are the Audit's. A teammate reads done while it rests (#83): its
//! newest turn end, an idle notice in its lead's transcript or a
//! TeammateIdle hook, is at or after its own last user or assistant record.
//! A worker of the drawn link that hangs under an earlier link's
//! orchestrator (a teammate's lead can be another link's) hangs under the
//! drawn one. The header's $ and run time sum the drawn link and the links
//! before it (#65).
//!
//! Spawned sessions (the model's `message` edges): a session's
//! orchestrator hangs under its first messager, an orchestrator or subagent
//! of another session, whatever either is named. The graph also draws each
//! chain an agent of the tab's links spawned, while that chain's drawn link
//! is live, with its running workers. Its orchestrator hangs (kind
//! `directs`) under its messager when that is drawn, else the messager's
//! nearest drawn ancestor, else the drawn orchestrator.
//!
//! A tab nests under another ([`Workstream::director`]) when a link of its
//! chain (the earliest, if several) was spawned from another live tab's
//! links: a chain nobody spawned keeps its own tab. A director is always a
//! top-level tab: a unit of a nested unit keeps its own tab, so no live
//! session is hidden twice over. A nested tab's needs-you and state count
//! toward its director's tab.

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
    /// Needs-you when anything in it (or nested under it) waits on the
    /// user, else the most urgent of the drawn link's orchestrator state
    /// and its nested tabs' states.
    pub state: AgentState,
    /// Waits in it and in the tabs nested under it.
    pub needs_you: i64,
    /// The session [`live_graph`] draws.
    pub session_id: Option<String>,
    /// Whether that session is live: the latest link of a process the
    /// registry lists as running.
    pub live: bool,
    /// The chain's latest link start.
    pub started_at: Option<String>,
    /// The live, top-level tab this one nests under: a link of its chain
    /// was spawned from that tab's links.
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
    /// The tab of the session's chain.
    pub workstream: Option<i64>,
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
    /// The dispatching or team-lead agent, or for a spawned session's
    /// orchestrator its messager, else their nearest drawn ancestor.
    pub parent: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    /// dispatch | teammate | directs (a spawned session's orchestrator
    /// under its messager).
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
/// last), never empty.
struct Chain {
    /// The process root, else the one session's id.
    root: String,
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
        let at = *tabs.by_root.entry(root.clone()).or_insert_with(|| {
            tabs.chains.push(Chain {
                root,
                links: Vec::new(),
            });
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

/// A subagent's or teammate's state: hooks first, then whether it finished
/// or rests (see [`session_agents`]) and whether its session is still live.
/// An idle hook is a turn end, which `finished` covers while no newer
/// record woke the teammate.
fn worker_state(needs_you: bool, hook: Option<&str>, finished: bool, live: bool) -> AgentState {
    match (needs_you, hook) {
        (true, _) => AgentState::NeedsYou,
        (_, Some("done")) => AgentState::Done,
        _ if finished || !live => AgentState::Done,
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

/// Each nested tab's director, from (unit, director) candidates: a tab
/// nests only under one that is no candidate's unit itself, so every
/// director is a top-level tab and no live session is hidden twice over (a
/// unit of a nested unit keeps its own tab; a cycle nests nothing). The
/// result does not depend on the candidates' order.
fn nest(candidates: impl IntoIterator<Item = (i64, i64)>) -> HashMap<i64, i64> {
    let candidates: HashMap<i64, i64> = candidates.into_iter().collect();
    candidates
        .iter()
        .filter(|(_, director)| !candidates.contains_key(director))
        .map(|(&unit, &director)| (unit, director))
        .collect()
}

/// Each tab that nests under another: a link of its chain was spawned (a
/// `message` edge) from a link of another tab whose drawn link is live,
/// itself nested under nothing ([`nest`]). With several, the earliest such
/// link's spawner is the one.
fn directors(
    conn: &Connection,
    tabs: &Tabs,
    listings: &HashMap<String, Listing>,
) -> Result<HashMap<i64, i64>> {
    let spawners: HashMap<i64, String> = conn
        .prepare_cached(
            "SELECT e.child, p.session_id FROM edges AS e JOIN agents AS p ON p.id = e.parent
             WHERE e.kind = 'message'",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let mut candidates = Vec::new();
    for chain in &tabs.chains {
        let director = chain.links.iter().find_map(|link| {
            let spawner = tabs.of_session(spawners.get(&link.orchestrator)?)?;
            (spawner.id() != chain.id() && spawner.drawn(listings).1).then(|| spawner.id())
        });
        if let Some(director) = director {
            candidates.push((chain.id(), director));
        }
    }
    Ok(nest(candidates))
}

/// Adds each nested tab's own needs-you count and state to its director's,
/// so a hidden unit's wait shows on the tab it nests under. Directors are
/// top-level, so one level is all there is.
fn fold_nested(rows: &mut [Workstream], directors: &HashMap<i64, i64>) {
    let at: HashMap<i64, usize> = rows.iter().enumerate().map(|(i, w)| (w.id, i)).collect();
    let own: Vec<(i64, i64, AgentState)> =
        rows.iter().map(|w| (w.id, w.needs_you, w.state)).collect();
    for (id, needs, state) in own {
        if let Some(&i) = directors.get(&id).and_then(|d| at.get(d)) {
            rows[i].needs_you += needs;
            rows[i].state = more_urgent(rows[i].state, state);
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

/// [`workstreams`]' name query for orchestrator `?1`: its effective
/// charge's key (as `agent_charges` resolves it, walking up from `?1`
/// alone), its name, cwd and branch.
const TAB_NAME_SQL: &str = "WITH RECURSIVE up (at, depth, seen) AS (
         SELECT ?1, 0, ',' || ?1 || ','
         UNION ALL
         SELECT p.parent, up.depth + 1, up.seen || p.parent || ','
         FROM up
         JOIN agents AS a ON a.id = up.at AND a.own_charge IS NULL
         JOIN spawn_parents AS p ON p.child = up.at
         WHERE up.depth < 32 AND instr(up.seen, ',' || p.parent || ',') = 0
     )
     SELECT (SELECT c.key FROM up JOIN agents AS h ON h.id = up.at
                 JOIN charges AS c ON c.id = h.own_charge
             ORDER BY up.depth LIMIT 1),
         a.name, a.cwd, a.git_branch
     FROM agents AS a WHERE a.id = ?1";

/// A tab's name and whether it is Unlinked: the charge key of `link`'s
/// orchestrator, else its name, else its listing's, else its cwd and
/// branch, else its session id.
fn tab_name(conn: &Connection, link: &Link, listing: Option<&Listing>) -> Result<(String, bool)> {
    type Named = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let (key, name, cwd, branch): Named = conn
        .prepare_cached(TAB_NAME_SQL)?
        .query_row([link.orchestrator], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
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
    let directors = directors(conn, &tabs, &listings)?;
    let costs = chain_costs(conn, &tabs)?;
    let mut needs: HashMap<i64, i64> = HashMap::new();
    for row in needs_you_in(conn, &tabs)? {
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
            director: directors.get(&id).copied(),
        });
    }
    fold_nested(&mut out, &directors);
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
/// names that session and counts toward its chain's tab.
///
/// One stop is one row (#84): a registry wait that started within
/// [`SAME_STOP_MS`] of a hook wait on its link (the orchestrator's or a
/// worker's: the registry describes the whole process) is that wait, and
/// the hook's row stays, since it names the tool. The view keeps both
/// signals, as recorded; every count of waits reads this list.
pub fn needs_you(conn: &Connection) -> Result<Vec<NeedsYou>> {
    needs_you_in(conn, &tabs(conn)?)
}

fn needs_you_in(conn: &Connection, tabs: &Tabs) -> Result<Vec<NeedsYou>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.source, n.session_id, n.agent_id, n.waiting_for, n.since_ms
         FROM needs_you AS n
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
/// A worker has finished once its dispatch is answered or its task
/// notification arrives. A background dispatch's tool_result is the
/// immediate launch result (`toolUseResult.status` `async_launched`), not
/// an answer: its end is the notification (#42). A teammate rests
/// ([`rests`]) from its newest idle notice in this session's own
/// transcript, the lead's, matched by its meta name, or idle hook.
/// The session's answers are read once ([`answered`]), not per agent: a
/// real session held 86 agents over thousands of records.
fn session_agents(
    conn: &Connection,
    session_id: &str,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Row>> {
    let listing = listings.get(session_id).unwrap_or(&NOT_LISTED);
    let notices = idle_notices(conn, session_id)?;
    let answered = answered(conn, session_id)?;
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.agent_id, a.kind, a.name, a.agent_type,
             a.description, a.model, a.cost_usd, a.context_tokens, a.context_pct,
             a.hook_state,
             EXISTS (SELECT 1 FROM needs_you AS n WHERE n.agent = a.id AND n.source = 'hook'),
             EXISTS (SELECT 1 FROM events AS n
                 WHERE n.session_id = a.session_id AND n.notification_task = a.agent_id),
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
             (SELECT d.tool_use_id FROM edges AS d WHERE d.child = a.id AND d.kind = 'dispatch')
         FROM agents AS a
         WHERE a.session_id = ?1
         ORDER BY a.agent_id IS NOT NULL, a.id",
    )?;
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
                let dispatch: Option<String> = r.get(17)?;
                let finished = r.get::<_, bool>(12)?
                    || dispatch.is_some_and(|d| answered.contains(&d))
                    || rests(idle, r.get(16)?);
                let hook: Option<String> = r.get(10)?;
                (
                    name,
                    worker_state(needs, hook.as_deref(), finished, listing.live),
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
            },
            edge,
        ))
    })?;
    rows.collect()
}

/// The tool_use ids `session_id`'s records answer: every tool_result but a
/// background dispatch's launch (`toolUseResult.status` `async_launched`).
fn answered(conn: &Connection, session_id: &str) -> Result<HashSet<String>> {
    conn.prepare_cached(
        "SELECT j.value FROM events AS e JOIN json_each(e.tool_results) AS j
         WHERE e.session_id = ?1 AND e.tool_results IS NOT NULL
           AND json_extract(e.raw, '$.toolUseResult.status') IS NOT 'async_launched'",
    )?
    .query_map([session_id], |r| r.get(0))?
    .collect()
}

/// A spawned chain to draw: its drawn link, and the agent that spawned it.
struct Unit {
    session_id: String,
    spawner: i64,
}

/// The chains any agent of `chain`'s links spawned (`message` edges), in
/// the spawned sessions' start order: each other chain's drawn link, once,
/// while it is live (#63: a finished unit is not drawn).
fn units(
    conn: &Connection,
    tabs: &Tabs,
    chain: &Chain,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Unit>> {
    let spawned: Vec<(i64, String)> = conn
        .prepare_cached(
            "SELECT e.parent, c.session_id FROM edges AS e
             JOIN agents AS p ON p.id = e.parent
             JOIN sessions AS ps ON ps.session_id = p.session_id
             JOIN agents AS c ON c.id = e.child
             JOIN sessions AS cs ON cs.session_id = c.session_id
             WHERE e.kind = 'message' AND ifnull(ps.process_root, ps.session_id) = ?1
             ORDER BY cs.started_at IS NULL, cs.started_at, cs.session_id",
        )?
        .query_map([&chain.root], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let mut out: Vec<Unit> = Vec::new();
    for (spawner, session_id) in spawned {
        let Some(unit) = tabs
            .of_session(&session_id)
            .filter(|u| u.id() != chain.id())
        else {
            continue;
        };
        let (link, live) = unit.drawn(listings);
        if live && !out.iter().any(|u| u.session_id == link.session_id) {
            out.push(Unit {
                session_id: link.session_id.clone(),
                spawner,
            });
        }
    }
    Ok(out)
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

/// The graph of the link tab `workstream` draws (any link's orchestrator id
/// reads as its tab), plus every live chain an agent of its links spawned:
/// that chain's orchestrator under its spawner by a `directs` edge, and its
/// subagents and teammates. No worker that reads done is drawn. `None` when
/// `workstream` is no tab's.
pub fn live_graph(conn: &Connection, workstream: i64) -> Result<Option<LiveGraph>> {
    let tabs = tabs(conn)?;
    let Some(&at) = tabs.by_orchestrator.get(&workstream) else {
        return Ok(None);
    };
    let chain = &tabs.chains[at];
    let listings = listings(conn)?;
    let (shown, live) = chain.drawn(&listings);
    let units = units(conn, &tabs, chain, &listings)?;
    let upto = chain
        .links
        .iter()
        .position(|l| l.session_id == shown.session_id)
        .unwrap_or(0);
    let links: Vec<&Link> = chain.links[..=upto].iter().collect();
    let (session_cost_usd, run_ms) = session_totals(conn, &links)?;
    let mut rows = chain_agents(conn, &shown.session_id, &listings)?;
    let director = orchestrator(&rows);
    for unit in &units {
        for (agent, edge) in chain_agents(conn, &unit.session_id, &listings)? {
            let edge = match agent.agent_id {
                None => Some((unit.spawner, "directs".to_string())),
                Some(_) => edge,
            };
            rows.push((agent, edge));
        }
    }
    let parents: HashMap<i64, i64> = rows
        .iter()
        .filter_map(|(a, edge)| Some((a.id, edge.as_ref()?.0)))
        .collect();
    // A finished worker is left to the Audit (#81).
    rows.retain(|(a, _)| a.agent_id.is_none() || a.state != AgentState::Done);
    let drawn_ids: Vec<i64> = rows.iter().map(|(a, _)| a.id).collect();
    let mut agents = Vec::with_capacity(rows.len());
    let mut edges = Vec::new();
    for (mut agent, edge) in rows {
        // One whose parent is not drawn (a finished dispatcher, a lead
        // outside the graph, or a spawner on an earlier link) hangs under
        // its nearest drawn ancestor, else the drawn orchestrator.
        let edge = edge.and_then(|(parent, kind)| {
            let parent = drawn_ancestor(parent, &parents, &drawn_ids).or(director)?;
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
        // An idle hook is a turn end the caller folds into `finished`
        // (#83): one it does not is behind a newer record.
        assert_eq!(worker_state(false, Some("idle"), false, true), Working);
        assert_eq!(worker_state(false, Some("running"), false, true), Working);
        assert_eq!(worker_state(false, None, false, true), Working);
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
    fn a_workstream_nests_only_under_one_that_nests_under_nothing() {
        let sorted = |m: HashMap<i64, i64>| {
            let mut v: Vec<_> = m.into_iter().collect();
            v.sort();
            v
        };
        // 1 -> 2 -> 3: 2 nests under 1; 3 keeps its tab.
        assert_eq!(sorted(nest([(3, 2), (2, 1)])), [(2, 1)]);
        assert_eq!(sorted(nest([(2, 1), (3, 2)])), [(2, 1)], "any order");
        // Siblings under one director.
        assert_eq!(sorted(nest([(2, 1), (3, 1)])), [(2, 1), (3, 1)]);
        // A cycle: no tab hides another.
        assert_eq!(sorted(nest([(1, 2), (2, 1)])), []);
        assert_eq!(sorted(nest([(1, 2), (2, 3), (3, 1), (4, 3)])), []);
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
                "contextPct",
                "contextTokens",
                "costUsd",
                "description",
                "id",
                "kind",
                "model",
                "name",
                "parent",
                "state"
            ]
        );
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
        });
        got.sort();
        assert_eq!(
            got,
            [
                "agentId",
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
