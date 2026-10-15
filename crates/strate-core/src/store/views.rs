//! Read views for the app: workstream tabs, needs-you, per-workstream $,
//! the live graph, and the Audit rollups, row timeline and performance read
//! ([`audit()`], [`audit_timeline()`], [`performance()`], from `store/audit/`).
//! Each runs on a read-only connection from
//! [`super::Store::open_reader`] and serializes in camelCase for the
//! webview.
//!
//! A registry row describes its process's `/clear` chain's latest link
//! (#63): `claude agents` keys a background process by the session it
//! started with, so after a `/clear` the row still names the first link
//! while the latest one runs. A session is live when a registry row of its
//! chain that is running makes it that latest link ([`listings`]), and
//! its orchestrator's registry name, state and wait come from that row. An
//! earlier link is never live, so a finished ticket whose process moved on
//! to the next has no tab. A background session `blocked` on the user is
//! not running either: no tab and no unit node, only a needs-you
//! ([`needs_you`]) on its latest link.
//!
//! A workstream draws one session: of the sessions with a segment in it, a
//! live one first, then the latest segment start. Its live graph is that
//! session's orchestrator plus the session's subagents and teammates that
//! joined one of the workstream's segments (or none yet), with dispatch and
//! teammate edges among them.
//!
//! A `/clear` chain (the sessions of one process root) reads as one
//! session, per workstream (#63): the graph also draws the subagents and
//! teammates of the drawn session's earlier links that have a segment in
//! the workstream, those that joined one of its segments, marked
//! [`GraphAgent::earlier`] (one with no segment reads in its link's current
//! workstream, so in one graph only). One that hung under its link's
//! orchestrator hangs under the drawn one. A link renamed into another
//! workstream reads there, never here: 35 of 62 continuation edges in a
//! real store crossed workstreams, a process reused for the next ticket.
//!
//! Director-launched units (#53, the `directs` table): a director's graph
//! also draws the units any of the workstream's sessions directed that are
//! still running (#63; a finished one is left to the Audit): per unit
//! chain, its latest link in the unit's workstream, when that link is
//! live, with its earlier links' agents in that workstream, as above. Each
//! unit's orchestrator hangs under the drawn orchestrator by a `directs`
//! edge. An agent is drawn once: a link the graph draws as itself (the
//! drawn session or a unit) is no other chain's earlier link, so its agents
//! hang under its own orchestrator; past that, the first reading wins (the
//! drawn chain, then the units in order).
//!
//! A workstream nests under a director's tab ([`Workstream::director`])
//! when any link of its drawn session's chain with a segment in it was
//! directed by a session of a live workstream (the earliest such link's,
//! if several): one that has since drawn a session of another chain, one
//! nobody directed, keeps its own tab, while the director's graph still
//! draws the directed one while it runs. A director is always a top-level
//! tab: a unit of a nested unit keeps its own tab, so no live session is
//! hidden twice over. A nested tab's needs-you and state count toward its
//! director's tab.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::Result;
pub use super::audit::{
    AgentRow, Audit, AuditFilter, AuditNode, Bottleneck, BottleneckDay, GithubError, GroupBy,
    Headline, LaneCount, Metrics, NodeKind, Outcome, OutcomeWeek, Peak, Performance,
    PermissionWaits, Progress, SpanKind, TimelineDay, TimelineMark, TimelineSpan, Tokens, Waste,
    audit, audit_timeline, performance,
};

/// An agent's state, as the canvas and tabs show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    NeedsYou,
    Idle,
    Done,
}

/// One orchestrator tab: a merge-resolved workstream.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workstream {
    pub id: i64,
    pub name: String,
    /// The name is the cwd/branch fallback, not a user-set name.
    pub fallback: bool,
    /// Sum of priced requests; `None` with none.
    pub cost_usd: Option<f64>,
    pub unpriced: i64,
    /// Needs-you when anything in it (or nested under it) waits on the
    /// user, else the most urgent of the drawn session's orchestrator state
    /// and its nested workstreams' states.
    pub state: AgentState,
    /// Waits in it and in the workstreams nested under it.
    pub needs_you: i64,
    /// The session [`live_graph`] draws.
    pub session_id: Option<String>,
    /// Whether that session is live: the latest link of a process the
    /// registry lists as running.
    pub live: bool,
    /// The latest segment start in the workstream.
    pub started_at: Option<String>,
    /// The live, top-level workstream whose tab this one nests under: its
    /// drawn session was directed by a session of that workstream.
    pub director: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkstreamCost {
    pub workstream: i64,
    pub cost_usd: Option<f64>,
    pub unpriced: i64,
}

/// One row of the `needs_you` view and the workstream it counts toward.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeedsYou {
    /// `registry` or `hook`.
    pub source: String,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub waiting_for: Option<String>,
    pub since_ms: Option<i64>,
    /// The session's current workstream, merge-resolved.
    pub workstream: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveGraph {
    pub session_id: String,
    pub live: bool,
    /// The drawn session's priced requests that count toward this
    /// workstream (`event_workstreams`); `None` with none.
    pub session_cost_usd: Option<f64>,
    /// The drawn session's wall time, first record to last (`sessions.run_ms`).
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
    /// The context percentage at which the agent is nudged: its own gear,
    /// else the orchestrator's, else the built-in 80.
    pub nudge_pct: u8,
    /// The context percentage at which the agent is stopped: its own gear,
    /// else the orchestrator's, else the built-in 95.
    pub hard_stop_pct: u8,
    /// The dispatching or team-lead agent, or for a unit's orchestrator the
    /// director's drawn orchestrator, when it is drawn too.
    pub parent: Option<i64>,
    /// Of an earlier link of the drawn session's `/clear` chain (or of a
    /// unit's): the canvas dims it. Left out of the JSON when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub earlier: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    /// dispatch | teammate | directs.
    pub kind: String,
    pub parent: i64,
    pub child: i64,
}

/// What the registry says of one session: the newest registry row whose
/// process's `/clear` chain has it as its latest link, a live row first
/// (as `live.rs` picks an orchestrator's `registry_state`).
struct Listing {
    /// The row is running: not gone, and not blocked on the user.
    live: bool,
    /// Its status or background state, `gone` once gone.
    state: Option<String>,
    name: Option<String>,
    /// It waits on the user (see [`REGISTRY_WAITS`]).
    waiting: bool,
}

/// A registry row `r` waiting on the user: not gone, and a session
/// `waiting` with something named to wait for (the v4 `needs_you` view's
/// registry rows), or a background session `blocked` (#63; the CLI lists
/// it with no pid, status or `waitingFor`).
const REGISTRY_WAITS: &str = "r.gone = 0 AND (r.status IS 'waiting' AND r.waiting_for IS NOT NULL
         OR r.state IS 'blocked')";

/// A session no registry row describes.
const NOT_LISTED: Listing = Listing {
    live: false,
    state: None,
    name: None,
    waiting: false,
};

/// The latest link (by start, then id) of the `/clear` chain of the
/// session `{session}` names: the sessions sharing its process root, or
/// having its id as theirs. The session itself when it has no row.
const LATEST_LINK: &str = "ifnull((SELECT s.session_id FROM sessions AS s
         WHERE s.session_id = {session} OR s.process_root = ifnull(
             (SELECT k.process_root FROM sessions AS k WHERE k.session_id = {session}),
             {session})
         ORDER BY s.started_at IS NULL, s.started_at DESC, s.session_id DESC LIMIT 1),
     {session})";

/// Every session the registry describes, by id: each row counts for its
/// chain's latest link, whichever link it names.
fn listings(conn: &Connection) -> Result<HashMap<String, Listing>> {
    let sql = format!(
        "SELECT {}, r.gone = 0 AND r.state IS NOT 'blocked',
             CASE WHEN r.gone = 1 THEN 'gone' ELSE coalesce(r.status, r.state) END, r.name,
             {REGISTRY_WAITS}
         FROM registry AS r WHERE r.session_id IS NOT NULL
         ORDER BY r.gone, r.last_seen_ms DESC, r.id DESC",
        LATEST_LINK.replace("{session}", "r.session_id")
    );
    let mut out = HashMap::new();
    let mut stmt = conn.prepare_cached(&sql)?;
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

/// Per workstream root: the session it draws, whether that one is live and
/// the workstream's latest segment start.
struct Drawn {
    session_id: String,
    live: bool,
    started_at: Option<String>,
}

/// The drawn session of every workstream root (or of the one `root`).
fn drawn(
    conn: &Connection,
    root: Option<i64>,
    listings: &HashMap<String, Listing>,
) -> Result<HashMap<i64, Drawn>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.root, g.session_id, max(g.started_at)
         FROM segments AS g JOIN workstream_roots AS r ON r.id = g.workstream_id
         WHERE ?1 IS NULL OR r.root = ?1
         GROUP BY r.root, g.session_id",
    )?;
    let rows = stmt.query_map([root], |r| {
        let session_id: String = r.get(1)?;
        Ok((
            r.get::<_, i64>(0)?,
            Drawn {
                live: is_live(listings, &session_id),
                session_id,
                started_at: r.get(2)?,
            },
        ))
    })?;
    let mut out: HashMap<i64, Drawn> = HashMap::new();
    let mut latest: HashMap<i64, Option<String>> = HashMap::new();
    for row in rows {
        let (root, d) = row?;
        let newest = latest.entry(root).or_default();
        if d.started_at > *newest {
            newest.clone_from(&d.started_at);
        }
        let better = out.get(&root).is_none_or(|best| {
            (d.live, &d.started_at, &best.session_id) > (best.live, &best.started_at, &d.session_id)
        });
        if better {
            out.insert(root, d);
        }
    }
    for (root, d) in &mut out {
        d.started_at = latest.remove(root).flatten();
    }
    Ok(out)
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
/// (see [`session_agents`]) and whether its session is still live.
fn worker_state(needs_you: bool, hook: Option<&str>, finished: bool, live: bool) -> AgentState {
    match (needs_you, hook) {
        (true, _) => AgentState::NeedsYou,
        (_, Some("done")) => AgentState::Done,
        _ if finished || !live => AgentState::Done,
        (_, Some("idle")) => AgentState::Idle,
        _ => AgentState::Working,
    }
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

/// Each nested workstream's director, from (unit, director) candidates: a
/// workstream nests only under one that is no candidate's unit itself, so
/// every director is a top-level tab and no live session is hidden twice
/// over (a unit of a nested unit keeps its own tab; a cycle nests nothing).
/// The result does not depend on the candidates' order.
fn nest(candidates: impl IntoIterator<Item = (i64, i64)>) -> HashMap<i64, i64> {
    let candidates: HashMap<i64, i64> = candidates.into_iter().collect();
    candidates
        .iter()
        .filter(|(_, director)| !candidates.contains_key(director))
        .map(|(&unit, &director)| (unit, director))
        .collect()
}

/// Each workstream that nests under another: any link of its drawn
/// session's chain with a segment in it was directed (`directs`) by a
/// session whose workstream is another, live one, itself nested under
/// nothing ([`nest`]). With several live directors, the earliest such
/// link's (by start, then session id) is the one, as the Audit nests a row
/// under its earliest directed link's director.
fn directors(conn: &Connection, drawn: &HashMap<i64, Drawn>) -> Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.root FROM sessions AS me
         JOIN sessions AS c ON c.session_id = me.session_id OR c.process_root = me.process_root
         JOIN directs AS x ON x.child = c.session_id
         JOIN sessions AS s ON s.session_id = x.parent
         JOIN workstream_roots AS r ON r.id = s.workstream_id
         WHERE me.session_id = ?1
           AND EXISTS (SELECT 1 FROM segments AS g
               JOIN workstream_roots AS gr ON gr.id = g.workstream_id
               WHERE g.session_id = c.session_id AND gr.root = ?2)
         ORDER BY c.started_at IS NULL, c.started_at, c.session_id",
    )?;
    let mut candidates = Vec::new();
    for (&root, shown) in drawn {
        let live_director = |d: &i64| *d != root && drawn.get(d).is_some_and(|x| x.live);
        let mut found = stmt.query_map(rusqlite::params![shown.session_id, root], |r| r.get(0))?;
        if let Some(d) = found.find(|d| d.as_ref().map_or(true, live_director)) {
            candidates.push((root, d?));
        }
    }
    Ok(nest(candidates))
}

/// Adds each nested workstream's own needs-you count and state to its
/// director's, so a hidden unit's wait shows on the tab it nests under.
/// Directors are top-level, so one level is all there is.
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

/// Every merge-resolved workstream, live first, then by latest start.
pub fn workstreams(conn: &Connection) -> Result<Vec<Workstream>> {
    let listings = listings(conn)?;
    let drawn = drawn(conn, None, &listings)?;
    let directors = directors(conn, &drawn)?;
    let mut needs: HashMap<i64, i64> = HashMap::new();
    for row in needs_you(conn)? {
        if let Some(w) = row.workstream {
            *needs.entry(w).or_default() += 1;
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT w.id, w.name, w.fallback, c.cost_usd, ifnull(c.unpriced, 0)
         FROM workstreams AS w LEFT JOIN workstream_costs AS c ON c.workstream = w.id
         WHERE w.merged_into IS NULL",
    )?;
    let mut out = Vec::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
            r.get::<_, Option<f64>>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (id, name, fallback, cost_usd, unpriced) = row?;
        let shown = drawn.get(&id);
        let registry_state = shown
            .and_then(|d| listings.get(&d.session_id))
            .and_then(|l| l.state.as_deref());
        let needs_you = needs.get(&id).copied().unwrap_or(0);
        out.push(Workstream {
            id,
            name,
            fallback,
            cost_usd,
            unpriced,
            state: orchestrator_state(needs_you > 0, registry_state),
            needs_you,
            session_id: shown.map(|d| d.session_id.clone()),
            live: shown.is_some_and(|d| d.live),
            started_at: shown.and_then(|d| d.started_at.clone()),
            director: directors.get(&id).copied(),
        });
    }
    fold_nested(&mut out, &directors);
    out.sort_by(|a, b| (b.live, &b.started_at, a.id).cmp(&(a.live, &a.started_at, b.id)));
    Ok(out)
}

/// $ per merge-resolved workstream (the `workstream_costs` view).
pub fn workstream_costs(conn: &Connection) -> Result<Vec<WorkstreamCost>> {
    let mut stmt = conn.prepare_cached(
        "SELECT workstream, cost_usd, unpriced FROM workstream_costs ORDER BY workstream",
    )?;
    stmt.query_map([], |r| {
        Ok(WorkstreamCost {
            workstream: r.get(0)?,
            cost_usd: r.get(1)?,
            unpriced: r.get(2)?,
        })
    })?
    .collect()
}

/// Everything waiting on the user, oldest first: the `needs_you` view's
/// hook rows, and the registry rows that wait ([`REGISTRY_WAITS`], which
/// adds blocked background sessions to the view's). A registry wait is its
/// process's latest link's (see [`listings`]), so it names that session
/// and counts toward its workstream.
pub fn needs_you(conn: &Connection) -> Result<Vec<NeedsYou>> {
    let sql = format!(
        "SELECT n.source, n.session_id, n.agent_id, n.waiting_for, n.since_ms, w.root
         FROM (SELECT 'registry' AS source, {} AS session_id, NULL AS agent_id,
                   r.waiting_for, r.last_seen_ms AS since_ms
               FROM registry AS r WHERE {REGISTRY_WAITS}
               UNION ALL
               SELECT source, session_id, agent_id, waiting_for, since_ms
               FROM needs_you WHERE source = 'hook') AS n
         LEFT JOIN sessions AS s ON s.session_id = n.session_id
         LEFT JOIN workstream_roots AS w ON w.id = s.workstream_id
         ORDER BY n.since_ms, n.session_id, n.agent_id",
        LATEST_LINK.replace("{session}", "r.session_id")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    stmt.query_map([], |r| {
        Ok(NeedsYou {
            source: r.get(0)?,
            session_id: r.get(1)?,
            agent_id: r.get(2)?,
            waiting_for: r.get(3)?,
            since_ms: r.get(4)?,
            workstream: r.get(5)?,
        })
    })?
    .collect()
}

/// A drawn agent and the edge to its dispatcher or team lead, if any.
type Row = (GraphAgent, Option<(i64, String)>);

/// The agents of `session_id`, orchestrator first: with `root`, only the
/// subagents and teammates that joined one of that workstream's segments.
/// One with no segment is a drawn session's worker starting now, so it
/// passes; an `earlier` link's has no window as evidence (its session was
/// grouped before it started, or its window was dropped), so it reads in
/// its session's current workstream, the one group.rs gives a worker with
/// no record yet, and so in one graph only. The orchestrator's name, state
/// and registry wait come from the session's listing ([`listings`]), and
/// workers' states read its liveness; an `earlier` link's are marked so.
///
/// A worker has finished once its dispatch is answered or its task
/// notification arrives. A background dispatch's tool_result is the
/// immediate launch result (`toolUseResult.status` `async_launched`), not
/// an answer: its end is the notification (#42).
fn session_agents(
    conn: &Connection,
    session_id: &str,
    root: Option<i64>,
    earlier: bool,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Row>> {
    let listing = listings.get(session_id).unwrap_or(&NOT_LISTED);
    // The orchestrator's `registry_name` and `registry_state` sit on the
    // link the registry names, not the one it describes, and a registry
    // wait is on that link's orchestrator too: both come from `listing`.
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.agent_id, a.kind, a.name, a.agent_type,
             a.description, a.model, a.cost_usd, a.context_tokens, a.context_pct,
             a.hook_state,
             EXISTS (SELECT 1 FROM needs_you AS n WHERE n.agent = a.id AND n.source = 'hook'),
             EXISTS (SELECT 1 FROM edges AS d
                 JOIN events AS e ON e.session_id = a.session_id AND e.tool_results IS NOT NULL
                 JOIN json_each(e.tool_results) AS j ON j.value = d.tool_use_id
                 WHERE d.child = a.id AND d.kind = 'dispatch'
                   AND json_extract(e.raw, '$.toolUseResult.status') IS NOT 'async_launched')
             OR EXISTS (SELECT 1 FROM events AS n
                 WHERE n.session_id = a.session_id AND n.notification_task = a.agent_id),
             (SELECT p.parent FROM edges AS p
                 WHERE p.child = a.id AND p.kind IN ('dispatch', 'teammate')),
             (SELECT p.kind FROM edges AS p
                 WHERE p.child = a.id AND p.kind IN ('dispatch', 'teammate')),
             ge.nudge_pct, ge.hard_stop_pct
         FROM agents AS a
         JOIN gear_effective AS ge ON ge.agent = a.id
         LEFT JOIN segments AS g ON g.id = a.segment_id
         LEFT JOIN sessions AS s ON s.session_id = a.session_id
         LEFT JOIN workstream_roots AS r ON r.id = CASE
             WHEN a.segment_id IS NOT NULL THEN g.workstream_id
             WHEN ?3 THEN s.workstream_id END
         WHERE a.session_id = ?1
           AND (?2 IS NULL OR a.agent_id IS NULL OR r.root = ?2
               OR (NOT ?3 AND a.segment_id IS NULL))
         ORDER BY a.agent_id IS NOT NULL, a.id",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id, root, earlier], |r| {
        let agent_id: Option<String> = r.get(1)?;
        let needs: bool = r.get(11)?;
        let (name, state) = match agent_id {
            None => (
                listing.name.clone().or(r.get(3)?),
                orchestrator_state(needs || listing.waiting, listing.state.as_deref()),
            ),
            Some(_) => (
                r.get(3)?,
                worker_state(
                    needs,
                    r.get::<_, Option<String>>(10)?.as_deref(),
                    r.get(12)?,
                    listing.live,
                ),
            ),
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
                nudge_pct: r.get(15)?,
                hard_stop_pct: r.get(16)?,
                parent: None,
                earlier,
            },
            edge,
        ))
    })?;
    rows.collect()
}

/// A unit to draw: its chain's latest link in its workstream.
struct Unit {
    session_id: String,
    /// The directed session's workstream (merge-resolved).
    workstream: Option<i64>,
}

/// The units any session of workstream `root` directed, other than `drawn`
/// and the workstream's own sessions, in the directed sessions' start
/// order: per unit chain and workstream, the latest link of the chain with
/// a segment in that workstream, once, while it is live (#63: a finished
/// unit is not drawn).
fn units(
    conn: &Connection,
    root: i64,
    drawn: &str,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Unit>> {
    let directed: Vec<(String, Option<i64>)> = conn
        .prepare_cached(
            "SELECT ifnull(c.process_root, c.session_id), cr.root
             FROM directs AS x
             JOIN sessions AS p ON p.session_id = x.parent
             JOIN workstream_roots AS pr ON pr.id = p.workstream_id
             JOIN sessions AS c ON c.session_id = x.child
             LEFT JOIN workstream_roots AS cr ON cr.id = c.workstream_id
             WHERE pr.root = ?1 AND x.child <> ?2 AND cr.root IS NOT ?1
             ORDER BY c.started_at IS NULL, c.started_at, x.child",
        )?
        .query_map(rusqlite::params![root, drawn], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_>>()?;
    let mut latest = conn.prepare_cached(
        "SELECT s.session_id FROM sessions AS s
         WHERE (s.process_root = ?1 OR s.session_id = ?1)
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM segments AS g
               JOIN workstream_roots AS r ON r.id = g.workstream_id
               WHERE g.session_id = s.session_id AND r.root = ?2))
         ORDER BY s.started_at IS NULL, s.started_at DESC, s.session_id DESC
         LIMIT 1",
    )?;
    let mut out: Vec<Unit> = Vec::new();
    for (chain, workstream) in directed {
        let session_id: String =
            latest.query_row(rusqlite::params![chain, workstream], |r| r.get(0))?;
        if session_id != drawn
            && is_live(listings, &session_id)
            && !out.iter().any(|u| u.session_id == session_id)
        {
            out.push(Unit {
                session_id,
                workstream,
            });
        }
    }
    Ok(out)
}

/// The earlier links of `session_id`'s `/clear` chain (its process root's
/// sessions that started before it), in start order; with `root`, only
/// those with a segment in that workstream.
fn earlier_links(conn: &Connection, session_id: &str, root: Option<i64>) -> Result<Vec<String>> {
    conn.prepare_cached(
        "SELECT s.session_id FROM sessions AS me
         JOIN sessions AS s ON s.process_root = me.process_root
         WHERE me.session_id = ?1
           AND (s.started_at, s.session_id) < (me.started_at, me.session_id)
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM segments AS g
               JOIN workstream_roots AS r ON r.id = g.workstream_id
               WHERE g.session_id = s.session_id AND r.root = ?2))
         ORDER BY s.started_at, s.session_id",
    )?
    .query_map(rusqlite::params![session_id, root], |r| r.get(0))?
    .collect()
}

/// `session_id`'s agents as [`session_agents`] reads them with `root`,
/// then the subagents and teammates of its earlier links in workstream
/// `links` (see [`earlier_links`]) that joined it, marked `earlier`. A link
/// in `drawn_sessions` (the graph draws it as itself: the drawn session or
/// a unit) is skipped, so its agents hang under their own orchestrator,
/// once. One that hangs under any link's orchestrator (a teammate's lead
/// can be another link's) hangs under `session_id`'s.
fn chain_agents(
    conn: &Connection,
    session_id: &str,
    links: Option<i64>,
    root: Option<i64>,
    listings: &HashMap<String, Listing>,
    drawn_sessions: &[&str],
) -> Result<Vec<Row>> {
    let mut rows = session_agents(conn, session_id, root, false, listings)?;
    let Some(head) = orchestrator(&rows) else {
        return Ok(rows);
    };
    for link in earlier_links(conn, session_id, links)? {
        if drawn_sessions.contains(&link.as_str()) {
            continue;
        }
        let workers = session_agents(conn, &link, links, true, listings)?;
        rows.extend(workers.into_iter().filter(|(a, _)| a.agent_id.is_some()));
    }
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

/// The orchestrator's id among `rows`.
fn orchestrator(rows: &[Row]) -> Option<i64> {
    rows.iter()
        .find(|(a, _)| a.agent_id.is_none())
        .map(|(a, _)| a.id)
}

/// The $ of `session_id` that counts toward workstream `root`, never more
/// than the workstream's total, and the session's run time.
fn session_totals(
    conn: &Connection,
    session_id: &str,
    root: i64,
) -> Result<(Option<f64>, Option<i64>)> {
    conn.prepare_cached(
        "SELECT (SELECT sum(r.cost_usd) FROM requests AS r
                 JOIN event_workstreams AS w ON w.event = r.id
                 WHERE r.session_id = ?1 AND w.workstream = ?2),
             (SELECT run_ms FROM sessions WHERE session_id = ?1)",
    )?
    .query_row(rusqlite::params![session_id, root], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
}

/// The graph of the session `workstream` draws (any id of a merge chain
/// resolves to its root) with its chain's earlier links' agents, plus every
/// unit any of the workstream's sessions directed: its orchestrator under
/// the drawn one by a `directs` edge, and its chain's subagents and
/// teammates. `None` when the workstream has no session.
pub fn live_graph(conn: &Connection, workstream: i64) -> Result<Option<LiveGraph>> {
    let root: Option<i64> = conn
        .prepare_cached("SELECT root FROM workstream_roots WHERE id = ?1")?
        .query_row([workstream], |r| r.get(0))
        .optional()?;
    let Some(root) = root else {
        return Ok(None);
    };
    let listings = listings(conn)?;
    let Some(shown) = drawn(conn, Some(root), &listings)?.remove(&root) else {
        return Ok(None);
    };
    let (session_cost_usd, run_ms) = session_totals(conn, &shown.session_id, root)?;
    let units = units(conn, root, &shown.session_id, &listings)?;
    let drawn_sessions: Vec<&str> = std::iter::once(shown.session_id.as_str())
        .chain(units.iter().map(|u| u.session_id.as_str()))
        .collect();
    let mut rows = chain_agents(
        conn,
        &shown.session_id,
        Some(root),
        Some(root),
        &listings,
        &drawn_sessions,
    )?;
    let director = orchestrator(&rows);
    for unit in &units {
        let unit_rows = chain_agents(
            conn,
            &unit.session_id,
            unit.workstream,
            None,
            &listings,
            &drawn_sessions,
        )?;
        for (agent, edge) in unit_rows {
            let edge = match (&agent.agent_id, director) {
                (None, Some(d)) => Some((d, "directs".to_string())),
                _ => edge,
            };
            rows.push((agent, edge));
        }
    }
    // An agent is drawn once, where it is first read: the drawn chain, then
    // each unit in order. Units are live, so no two are of one chain; what
    // can still repeat is a unit of the drawn session's own chain with no
    // workstream (a directed link with no window yet), which reads its
    // chain's earlier links in every workstream.
    let mut seen = HashSet::new();
    rows.retain(|(a, _)| seen.insert(a.id));
    let drawn_ids: Vec<i64> = rows.iter().map(|(a, _)| a.id).collect();
    let mut agents = Vec::with_capacity(rows.len());
    let mut edges = Vec::new();
    for (mut agent, edge) in rows {
        if let Some((parent, kind)) = edge.filter(|(p, _)| drawn_ids.contains(p)) {
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
        live: shown.live,
        session_id: shown.session_id,
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
        assert_eq!(worker_state(false, Some("idle"), false, true), Idle);
        assert_eq!(worker_state(false, Some("running"), false, true), Working);
        assert_eq!(worker_state(false, None, false, true), Working);
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

    /// A store at `name` with `sql` run on it.
    fn seeded(name: &str, sql: &str) -> (std::path::PathBuf, super::super::Store) {
        let dir = crate::tail::test_dir(name);
        let store = super::super::Store::open_in(dir.join("strate.db"), "UTC").expect("open");
        store.conn.execute_batch(sql).expect("seed");
        (dir, store)
    }

    /// Workstream 1 (`demo`) holds `p` (10:00) and `c` (11:00), and `p`
    /// directed `c`; `c` was then renamed into workstream 2 (`other`, 12:00).
    /// `c` is live, so workstream 1 draws it.
    const SAME_WORKSTREAM: &str = "
        INSERT INTO workstreams (id, name) VALUES (1, 'demo'), (2, 'other');
        INSERT INTO sessions (session_id, workstream_id) VALUES ('p', 1), ('c', 2);
        INSERT INTO agents (session_id, kind) VALUES ('p', 'orchestrator'), ('c', 'orchestrator');
        INSERT INTO segments (session_id, start_offset, started_at, workstream_id) VALUES
            ('p', 0, '2026-10-07T10:00:00.000Z', 1),
            ('c', 0, '2026-10-07T11:00:00.000Z', 1),
            ('c', 500, '2026-10-07T12:00:00.000Z', 2);
        INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status, first_seen_ms,
            last_seen_ms) VALUES ('pid:1:1', 'interactive', 'c', '/work/x', 1, 'busy', 1, 1);
        INSERT INTO directs (child, parent, msg_id) VALUES ('c', 'p', 'm1');";

    #[test]
    fn a_session_directed_from_its_own_workstream_nests_nowhere_and_is_drawn_once() {
        let (dir, store) = seeded("views-same-workstream", SAME_WORKSTREAM);
        let rows = workstreams(&store.conn).expect("workstreams");
        let demo = rows.iter().find(|w| w.id == 1).expect("demo");
        assert_eq!(demo.session_id.as_deref(), Some("c"));
        assert_eq!(demo.director, None, "never under itself");
        // `other` draws c too, directed from live `demo`.
        assert_eq!(
            rows.iter().find(|w| w.id == 2).expect("other").director,
            Some(1)
        );
        // c is demo's drawn session, not a unit drawn again beside it.
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        assert_eq!(graph.agents.len(), 1, "{:?}", graph.agents);
        assert!(graph.edges.is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_teammate_led_from_another_link_hangs_under_the_drawn_orchestrator() {
        // p1 then p2: one process (root p1) in workstream 1. The team lead
        // is p1's orchestrator; one teammate's transcript is p2's, the
        // other's p1's.
        let (dir, store) = seeded(
            "views-chain-teammates",
            "INSERT INTO workstreams (id, name) VALUES (1, 'demo');
             INSERT INTO sessions (session_id, workstream_id, process_root, started_at) VALUES
                 ('p1', 1, 'p1', '2026-10-07T10:00:00.000Z'),
                 ('p2', 1, 'p1', '2026-10-07T11:00:00.000Z');
             INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
                 (1, 'p1', 0, '2026-10-07T10:00:00.000Z', 1),
                 (2, 'p2', 0, '2026-10-07T11:00:00.000Z', 1);
             INSERT INTO agents (id, session_id, agent_id, kind, segment_id) VALUES
                 (1, 'p1', NULL, 'orchestrator', NULL), (2, 'p2', NULL, 'orchestrator', NULL),
                 (3, 'p2', 'm1', 'teammate', 2), (4, 'p1', 'm2', 'teammate', 1);
             INSERT INTO edges (kind, parent, child) VALUES ('teammate', 1, 3), ('teammate', 1, 4);",
        );
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        assert_eq!(graph.session_id, "p2");
        let drawn: Vec<(i64, Option<i64>, bool)> = graph
            .agents
            .iter()
            .map(|a| (a.id, a.parent, a.earlier))
            .collect();
        assert_eq!(
            drawn,
            [(2, None, false), (3, Some(2), false), (4, Some(2), true)]
        );
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_directed_session_of_the_drawn_workstream_is_not_a_unit() {
        // p is live and drawn; c, directed by p, is an earlier session of
        // the same workstream.
        let (dir, store) = seeded(
            "views-own-unit",
            "INSERT INTO workstreams (id, name) VALUES (1, 'demo');
             INSERT INTO sessions (session_id, workstream_id) VALUES ('p', 1), ('c', 1);
             INSERT INTO agents (session_id, kind) VALUES ('p', 'orchestrator'),
                 ('c', 'orchestrator');
             INSERT INTO segments (session_id, start_offset, started_at, workstream_id) VALUES
                 ('p', 0, '2026-10-07T10:00:00.000Z', 1),
                 ('c', 0, '2026-10-07T09:00:00.000Z', 1);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'p', '/work/x', 1, 'busy', 1, 1);
             INSERT INTO directs (child, parent, msg_id) VALUES ('c', 'p', 'm1');",
        );
        let graph = live_graph(&store.conn, 1).expect("query").expect("graph");
        assert_eq!(graph.session_id, "p");
        assert_eq!(graph.agents.len(), 1, "{:?}", graph.agents);
        assert!(graph.edges.is_empty());
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
            nudge_pct: 80,
            hard_stop_pct: 95,
            parent: None,
            earlier: false,
        };
        assert_eq!(
            serde_json::to_value(agent.state).expect("json"),
            "needs_you"
        );
        let earlier = GraphAgent {
            earlier: true,
            ..agent.clone()
        };
        assert_eq!(
            serde_json::to_value(&earlier).expect("json")["earlier"],
            true
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
                "hardStopPct",
                "id",
                "kind",
                "model",
                "name",
                "nudgePct",
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
