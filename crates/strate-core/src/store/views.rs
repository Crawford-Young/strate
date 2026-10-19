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
//! ([`needs_you`]) on its latest link. A hook's needs-you is on a latest
//! link only (#68): a `/clear` left an earlier link's prompt behind. A
//! registry wait and a hook wait on one stop are one needs-you row, the
//! hook's (#84).
//!
//! A workstream draws one session: of the sessions with a segment in it, a
//! live one first, then the latest segment start. Its live graph is that
//! session's orchestrator plus the session's subagents and teammates that
//! joined one of the workstream's segments (or none yet) and still run,
//! with dispatch and teammate edges among them. A worker that reads done is
//! not drawn (#81): finished workers are the Audit's. A teammate reads done
//! while it rests (#83): its newest turn end, an idle notice in its lead's
//! transcript or a TeammateIdle hook, is at or after its own last user or
//! assistant record.
//!
//! A `/clear` chain (the sessions of one process root) reads as one
//! session, per workstream (#63). An earlier link is never live, so its
//! workers read done and none is drawn; a worker of the drawn session that
//! hangs under an earlier link's orchestrator (a teammate's lead can be
//! another link's) hangs under the drawn one. A link renamed into another
//! workstream reads there, never here: 35 of 62 continuation edges in a
//! real store crossed workstreams, a process reused for the next ticket.
//! The header's $ and run time sum the drawn session and its earlier links
//! in the workstream (#65).
//!
//! Director-launched units (#53, the `directs` table): a director's graph
//! also draws the units any of the workstream's sessions directed that are
//! still running (#63; a finished one is left to the Audit): per unit
//! chain, its latest link in the unit's workstream, when that link is
//! live, with its running workers. Each unit's orchestrator hangs under the
//! drawn orchestrator by a `directs` edge.
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

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::Result;
pub use super::audit::{
    AgentRow, Audit, AuditFilter, AuditNode, Bottleneck, BottleneckDay, GithubError, Headline,
    LaneCount, Metrics, NodeKind, Outcome, OutcomeDay, Peak, Performance, PermissionWaits,
    Progress, SpanKind, TimelineDay, TimelineMark, TimelineSpan, Tokens, Waste, audit,
    audit_timeline, performance,
};
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
    /// The priced requests of the drawn session and its chain's earlier
    /// links in it that count toward this workstream
    /// (`event_workstreams`); `None` with none.
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
    /// The context percentage at which the agent is nudged: its own gear,
    /// else the orchestrator's, else the built-in 80.
    pub nudge_pct: u8,
    /// The context percentage at which the agent is stopped: its own gear,
    /// else the orchestrator's, else the built-in 95.
    pub hard_stop_pct: u8,
    /// The dispatching or team-lead agent, or for a unit's orchestrator the
    /// director's drawn orchestrator, when it is drawn too.
    pub parent: Option<i64>,
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
    let mut stmt = conn.prepare_cached(DIRECTORS_SQL)?;
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

/// [`directors`]' query: the workstream of each session that directed a
/// link of drawn session `?1`'s chain with a segment in workstream `?2`.
const DIRECTORS_SQL: &str = "SELECT r.root FROM sessions AS me
     JOIN sessions AS c ON c.session_id = me.session_id OR c.process_root = me.process_root
     JOIN directs AS x ON x.child = c.session_id
     JOIN sessions AS s ON s.session_id = x.parent
     JOIN workstream_roots AS r ON r.id = s.workstream_id
     WHERE me.session_id = ?1
       AND EXISTS (SELECT 1 FROM segments AS g
           JOIN workstream_roots AS gr ON gr.id = g.workstream_id
           WHERE g.session_id = c.session_id AND gr.root = ?2)
     ORDER BY c.started_at IS NULL, c.started_at, c.session_id";

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
    let mut stmt = conn.prepare_cached(WORKSTREAMS_SQL)?;
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

/// [`workstreams`]' query: each merge root and its $.
const WORKSTREAMS_SQL: &str = "SELECT w.id, w.name, w.fallback, c.cost_usd, ifnull(c.unpriced, 0)
     FROM workstreams AS w LEFT JOIN workstream_costs AS c ON c.workstream = w.id
     WHERE w.merged_into IS NULL";

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

/// How far apart a registry wait and a hook wait can start and still be
/// one stop. The poller runs every second and stamps a poll before it
/// runs; the real store's pairs were 23 to 375 ms apart. A registry wait
/// starts at the poll that first saw it waiting (`waiting_since_ms`).
const SAME_STOP_MS: i64 = 5_000;

/// Everything waiting on the user, oldest first: the `needs_you` view, on
/// latest links (#68). A registry wait is its process's latest link's (see
/// [`listings`]), and a hook wait is listed only on a latest link, so each
/// names that session and counts toward its workstream.
///
/// One stop is one row (#84): a registry wait that started within
/// [`SAME_STOP_MS`] of a hook wait on its link (the orchestrator's or a
/// worker's: the registry describes the whole process) is that wait, and
/// the hook's row stays, since it names the tool. The view keeps both
/// signals, as recorded; every count of waits reads this list.
pub fn needs_you(conn: &Connection) -> Result<Vec<NeedsYou>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.source, n.session_id, n.agent_id, n.waiting_for, n.since_ms, w.root
         FROM needs_you AS n
         LEFT JOIN sessions AS s ON s.session_id = n.session_id
         LEFT JOIN workstream_roots AS w ON w.id = s.workstream_id
         WHERE n.source = 'hook' OR NOT EXISTS (SELECT 1 FROM needs_you AS h
             WHERE h.source = 'hook' AND h.session_id = n.session_id
               AND abs(h.since_ms - n.since_ms) <= ?1)
         ORDER BY n.since_ms, n.session_id, n.agent_id",
    )?;
    stmt.query_map([SAME_STOP_MS], |r| {
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
/// One with no segment is a worker starting now, so it passes. The
/// orchestrator's name, state and registry wait come from the session's
/// listing ([`listings`]), and workers' states read its liveness.
///
/// A worker has finished once its dispatch is answered or its task
/// notification arrives. A background dispatch's tool_result is the
/// immediate launch result (`toolUseResult.status` `async_launched`), not
/// an answer: its end is the notification (#42). A teammate rests
/// ([`rests`]) from its newest idle notice in this session's own
/// transcript, the lead's, matched by its meta name, or idle hook.
fn session_agents(
    conn: &Connection,
    session_id: &str,
    root: Option<i64>,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Row>> {
    let listing = listings.get(session_id).unwrap_or(&NOT_LISTED);
    let notices = idle_notices(conn, session_id)?;
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
             ge.nudge_pct, ge.hard_stop_pct,
             CASE WHEN a.hook_state = 'idle' THEN a.hook_at_ms END,
             CASE WHEN a.kind = 'teammate' OR a.hook_state = 'idle' THEN
                 (SELECT CAST(round(max(unixepoch(e.timestamp, 'subsec')) * 1000) AS INTEGER)
                  FROM events AS e
                  WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id
                    AND e.type IN ('user', 'assistant'))
             END
         FROM agents AS a
         JOIN gear_effective AS ge ON ge.agent = a.id
         LEFT JOIN segments AS g ON g.id = a.segment_id
         LEFT JOIN workstream_roots AS r ON r.id = g.workstream_id
         WHERE a.session_id = ?1
           AND (?2 IS NULL OR a.agent_id IS NULL OR a.segment_id IS NULL OR r.root = ?2)
         ORDER BY a.agent_id IS NOT NULL, a.id",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id, root], |r| {
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
                let idle = notice.max(r.get(17)?);
                let finished = r.get::<_, bool>(12)? || rests(idle, r.get(18)?);
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
                nudge_pct: r.get(15)?,
                hard_stop_pct: r.get(16)?,
                parent: None,
            },
            edge,
        ))
    })?;
    rows.collect()
}

/// A unit to draw: its chain's latest link in its workstream.
struct Unit {
    session_id: String,
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
            out.push(Unit { session_id });
        }
    }
    Ok(out)
}

/// The earlier links of `session_id`'s `/clear` chain (its process root's
/// sessions that started before it) that the header sums, in start order:
/// with `root`, only those with a segment in that workstream, and never one
/// in `drawn_sessions` (the graph draws it as itself: the drawn session or a
/// unit).
fn earlier_links(
    conn: &Connection,
    session_id: &str,
    root: Option<i64>,
    drawn_sessions: &[&str],
) -> Result<Vec<String>> {
    let links: Vec<String> = conn
        .prepare_cached(
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
        .collect::<Result<_>>()?;
    Ok(links
        .into_iter()
        .filter(|l| !drawn_sessions.contains(&l.as_str()))
        .collect())
}

/// `session_id`'s agents as [`session_agents`] reads them with `root`.
/// One that hangs under the orchestrator of any link of its `/clear` chain
/// (a teammate's lead can be another link's) hangs under `session_id`'s.
fn chain_agents(
    conn: &Connection,
    session_id: &str,
    root: Option<i64>,
    listings: &HashMap<String, Listing>,
) -> Result<Vec<Row>> {
    let mut rows = session_agents(conn, session_id, root, listings)?;
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

/// The $ of the chain links `links` that counts toward workstream `root`,
/// never more than the workstream's total, and the sum of their run times
/// (#65: the header covers the links the canvas draws).
fn session_totals(
    conn: &Connection,
    links: &[String],
    root: i64,
) -> Result<(Option<f64>, Option<i64>)> {
    fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT (SELECT sum(r.cost_usd) FROM requests AS r
                 JOIN event_workstreams AS w ON w.event = r.id
                 WHERE r.session_id = ?1 AND w.workstream = ?2),
             (SELECT run_ms FROM sessions WHERE session_id = ?1)",
    )?;
    let mut totals = (None, None);
    for link in links {
        let (usd, run): (Option<f64>, Option<i64>) = stmt
            .query_row(rusqlite::params![link, root], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        totals = (add(totals.0, usd), add(totals.1, run));
    }
    Ok(totals)
}

/// The graph of the session `workstream` draws (any id of a merge chain
/// resolves to its root), plus every unit any of the workstream's sessions
/// directed: its orchestrator under the drawn one by a `directs` edge, and
/// its subagents and teammates. No worker that reads done is drawn. `None`
/// when the workstream has no session.
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
    let units = units(conn, root, &shown.session_id, &listings)?;
    let drawn_sessions: Vec<&str> = std::iter::once(shown.session_id.as_str())
        .chain(units.iter().map(|u| u.session_id.as_str()))
        .collect();
    let mut links = vec![shown.session_id.clone()];
    links.extend(earlier_links(
        conn,
        &shown.session_id,
        Some(root),
        &drawn_sessions,
    )?);
    let (session_cost_usd, run_ms) = session_totals(conn, &links, root)?;
    let mut rows = chain_agents(conn, &shown.session_id, Some(root), &listings)?;
    let director = orchestrator(&rows);
    for unit in &units {
        let unit_rows = chain_agents(conn, &unit.session_id, None, &listings)?;
        for (agent, edge) in unit_rows {
            let edge = match (&agent.agent_id, director) {
                (None, Some(d)) => Some((d, "directs".to_string())),
                _ => edge,
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
        // One whose parent is not drawn (a finished dispatcher, or a lead
        // outside the graph) hangs under its nearest drawn ancestor, else
        // the drawn orchestrator.
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
    use std::collections::HashSet;

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

    /// `workstreams()` runs [`directors`]' query once per drawn workstream:
    /// it finds the drawn session's chain by the session key and the
    /// process-root index, each link's director by key, and scans no
    /// table but `workstream_roots`' merge walk over `workstreams` (#67).
    #[test]
    fn a_tabs_director_is_read_by_index() {
        let dir = crate::tail::test_dir("directors-plan");
        let db = dir.join("strate.db");
        drop(super::super::Store::open(&db).expect("open"));
        let conn = super::super::Store::open_reader(&db).expect("reader");
        let plan = super::super::query_plan(&conn, DIRECTORS_SQL);
        let on = |table: &[&str]| -> Vec<&str> {
            plan.iter()
                .map(String::as_str)
                .filter(|step| table.iter().any(|t| step.split(' ').nth(1) == Some(t)))
                .collect()
        };
        assert_eq!(
            on(&["me", "c", "x", "s"]),
            [
                "SEARCH me USING INDEX sqlite_autoindex_sessions_1 (session_id=?)",
                "SEARCH c USING INDEX sqlite_autoindex_sessions_1 (session_id=?)",
                "SEARCH c USING INDEX sessions_process_root (process_root=?)",
                "SEARCH x USING INDEX sqlite_autoindex_directs_1 (child=?)",
                "SEARCH s USING INDEX sqlite_autoindex_sessions_1 (session_id=?)",
            ],
            "{plan:?}"
        );
        let scans = plan.iter().filter(|step| step.starts_with("SCAN "));
        assert_eq!(
            scans.collect::<HashSet<_>>(),
            HashSet::from([&"SCAN workstreams".to_string(), &"SCAN r".to_string()]),
            "{plan:?}"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    /// `workstreams()` sums every request on each call, so its cost lookup
    /// reads events from `events_requests` alone: one pass over the index,
    /// one search per request for a fuller copy, and no row read (#79).
    #[test]
    fn a_tabs_cost_is_read_from_the_request_index() {
        let dir = crate::tail::test_dir("workstreams-plan");
        let db = dir.join("strate.db");
        drop(super::super::Store::open(&db).expect("open"));
        let conn = super::super::Store::open_reader(&db).expect("reader");
        let plan = super::super::query_plan(&conn, WORKSTREAMS_SQL);
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

    /// Session `p`'s orchestrator records split at offset 500 between
    /// workstreams 1 and 2; its subagent `sub` joined 2's segment and `lost`
    /// none. `q`'s one segment is in 3, merged into 1. Each request record
    /// is (offset, output tokens, $), of model `m` (an upgrade's rollup
    /// reads one): `k3` streamed twice, `k4` is
    /// unpriced, and offset 50 precedes p's first segment.
    const SPLIT_COSTS: &str = "
        INSERT INTO workstreams (id, name, merged_into) VALUES
            (1, 'a', NULL), (2, 'b', NULL), (3, 'c', 1);
        INSERT INTO sessions (session_id, workstream_id) VALUES ('p', 2), ('q', 3);
        INSERT INTO segments (id, session_id, start_offset, workstream_id) VALUES
            (10, 'p', 100, 1), (11, 'p', 500, 2), (12, 'q', 0, 3);
        INSERT INTO agents (session_id, agent_id, kind, segment_id) VALUES
            ('p', NULL, 'orchestrator', NULL), ('p', 'sub', 'subagent', 11),
            ('p', 'lost', 'subagent', NULL), ('q', NULL, 'orchestrator', NULL);
        INSERT INTO events (session_id, agent_id, file, byte_offset, raw, request_key, model,
            usage, output_tokens, cost_usd) VALUES
            ('p', NULL, 'p', 50, '{}', 'k1', 'm', '{}', 1, 1.0),
            ('p', NULL, 'p', 100, '{}', 'k2', 'm', '{}', 1, 2.0),
            ('p', NULL, 'p', 150, '{}', NULL, NULL, NULL, NULL, NULL),
            ('p', NULL, 'p', 200, '{}', 'k3', 'm', '{}', 5, 4.0),
            ('p', NULL, 'p', 250, '{}', 'k3', 'm', '{}', 9, 8.0),
            ('p', NULL, 'p', 600, '{}', 'k4', 'm', '{}', 1, NULL),
            ('p', 'sub', 'p-sub', 0, '{}', 'k5', 'm', '{}', 1, 16.0),
            ('p', 'lost', 'p-lost', 0, '{}', 'k6', 'm', '{}', 1, 32.0),
            ('q', NULL, 'q', 10, '{}', 'k7', 'm', '{}', 1, 64.0);";

    #[test]
    fn a_request_costs_where_its_record_falls() {
        let (dir, store) = seeded("views-split-costs", SPLIT_COSTS);
        let costs: Vec<(i64, Option<f64>, i64)> = workstream_costs(&store.conn)
            .expect("costs")
            .into_iter()
            .map(|c| (c.workstream, c.cost_usd, c.unpriced))
            .collect();
        // 1: p before 500 (k2, k3's fuller record) and merged-in q; 2: p
        // after 500 and sub. k1 and lost have no segment.
        assert_eq!(costs, [(1, Some(74.0), 0), (2, Some(16.0), 1)]);
        let tabs: Vec<(i64, Option<f64>, i64)> = workstreams(&store.conn)
            .expect("workstreams")
            .into_iter()
            .map(|w| (w.id, w.cost_usd, w.unpriced))
            .collect();
        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert!(tabs.contains(&(1, Some(74.0), 0)), "{tabs:?}");
        assert!(tabs.contains(&(2, Some(16.0), 1)), "{tabs:?}");
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_v14_store_keeps_its_workstream_costs() {
        let dir = crate::tail::test_dir("views-v14-costs");
        let db = dir.join("strate.db");
        let mut v14 = Connection::open(&db).expect("v14 db");
        super::super::migrate::migrate(&mut v14, &super::super::migrate::MIGRATIONS[..14])
            .expect("v14");
        v14.execute_batch(SPLIT_COSTS).expect("v14 rows");
        let before: Vec<(i64, Option<f64>, i64)> = v14
            .prepare("SELECT workstream, cost_usd, unpriced FROM workstream_costs ORDER BY 1")
            .expect("v14 costs")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("rows")
            .collect::<Result<_>>()
            .expect("costs");
        drop(v14);

        let store = super::super::Store::open_in(&db, "UTC").expect("upgrade");
        let after: Vec<(i64, Option<f64>, i64)> = workstream_costs(&store.conn)
            .expect("costs")
            .into_iter()
            .map(|c| (c.workstream, c.cost_usd, c.unpriced))
            .collect();
        assert_eq!(before, [(1, Some(74.0), 0), (2, Some(16.0), 1)]);
        assert_eq!(after, before);
        drop(store);
        std::fs::remove_dir_all(dir).expect("cleanup");
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
        // p1 then p2: one live process (root p1) in workstream 1. The team
        // lead is p1's orchestrator; one teammate's transcript is p2's, the
        // other's p1's, so it reads done and is not drawn.
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
             INSERT INTO edges (kind, parent, child) VALUES ('teammate', 1, 3), ('teammate', 1, 4);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'p2', '/work/x', 1, 'busy', 1, 1);",
        );
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
        // p1 then p2: one live process (root p1) in workstream 1, which
        // directed the live unit u. Each of p2 and u has a running and a
        // finished worker; p1's teammate is an earlier link's.
        let (dir, store) = seeded(
            "views-no-done-workers",
            "INSERT INTO workstreams (id, name) VALUES (1, 'demo'), (2, 'ticket');
             INSERT INTO sessions (session_id, workstream_id, process_root, started_at) VALUES
                 ('p1', 1, 'p1', '2026-10-07T10:00:00.000Z'),
                 ('p2', 1, 'p1', '2026-10-07T11:00:00.000Z'),
                 ('u', 2, 'u', '2026-10-07T10:30:00.000Z');
             INSERT INTO segments (id, session_id, start_offset, started_at, workstream_id) VALUES
                 (1, 'p1', 0, '2026-10-07T10:00:00.000Z', 1),
                 (2, 'p2', 0, '2026-10-07T11:00:00.000Z', 1),
                 (3, 'u', 0, '2026-10-07T10:30:00.000Z', 2);
             INSERT INTO agents (id, session_id, agent_id, kind, segment_id, hook_state) VALUES
                 (1, 'p1', NULL, 'orchestrator', NULL, NULL),
                 (2, 'p2', NULL, 'orchestrator', NULL, NULL),
                 (3, 'p2', 'w1', 'subagent', 2, NULL), (4, 'p2', 'w2', 'subagent', 2, 'done'),
                 (5, 'p1', 'm1', 'teammate', 1, NULL),
                 (6, 'u', NULL, 'orchestrator', NULL, NULL),
                 (7, 'u', 'w3', 'subagent', 3, NULL), (8, 'u', 'w4', 'subagent', 3, 'done');
             INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                 ('dispatch', 2, 3, 't1'), ('dispatch', 2, 4, 't2'), ('teammate', 1, 5, NULL),
                 ('dispatch', 6, 7, 't3'), ('dispatch', 6, 8, 't4');
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms) VALUES
                 ('pid:1:1', 'interactive', 'p2', '/work/x', 1, 'busy', 1, 1),
                 ('pid:2:1', 'interactive', 'u', '/work/x', 1, 'busy', 1, 1);
             INSERT INTO directs (child, parent, msg_id) VALUES ('u', 'p2', 'm1');",
        );
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
    fn a_running_worker_under_a_hidden_parent_hangs_under_its_nearest_drawn_ancestor() {
        // p is live: its subagent 2 finished, but 2's own dispatch 3 runs,
        // and so does 4, a teammate whose lead is q's undrawn orchestrator.
        let (dir, store) = seeded(
            "views-hidden-parent",
            "INSERT INTO workstreams (id, name) VALUES (1, 'demo'), (2, 'other');
             INSERT INTO sessions (session_id, workstream_id) VALUES ('p', 1), ('q', 2);
             INSERT INTO segments (session_id, start_offset, started_at, workstream_id) VALUES
                 ('p', 0, '2026-10-07T10:00:00.000Z', 1), ('q', 0, '2026-10-07T09:00:00.000Z', 2);
             INSERT INTO agents (id, session_id, agent_id, kind, hook_state) VALUES
                 (1, 'p', NULL, 'orchestrator', NULL), (2, 'p', 'w1', 'subagent', 'done'),
                 (3, 'p', 'w2', 'subagent', NULL), (4, 'p', 'm1', 'teammate', NULL),
                 (5, 'q', NULL, 'orchestrator', NULL);
             INSERT INTO edges (kind, parent, child, tool_use_id) VALUES
                 ('dispatch', 1, 2, 't1'), ('dispatch', 2, 3, 't2'), ('teammate', 5, 4, NULL);
             INSERT INTO registry (key, kind, session_id, cwd, started_at_ms, status,
                 first_seen_ms, last_seen_ms)
             VALUES ('pid:1:1', 'interactive', 'p', '/work/x', 1, 'busy', 1, 1);",
        );
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
    fn a_registry_wait_and_a_hook_wait_on_one_stop_are_one_row() {
        // Each session has a hook wait from 10000 and a registry wait whose
        // start differs: s by 375 ms (the real store's gap), u by 5000 ms,
        // v by 5001 ms, x on a subagent by 2000 ms, b blocked in the
        // background. g's registry row is gone, so g needs nothing. A
        // wait's start is never its row's last change (99999).
        let (dir, store) = seeded(
            "views-one-stop",
            "INSERT INTO workstreams (id, name) VALUES (1, 'demo'), (2, 'other');
             INSERT INTO sessions (session_id, workstream_id) VALUES
                 ('s', 1), ('u', NULL), ('v', 2), ('x', NULL), ('b', NULL),
                 ('g', NULL);
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
                     10000);",
        );
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
        let counts: Vec<(i64, i64, AgentState)> = workstreams(&store.conn)
            .expect("workstreams")
            .iter()
            .map(|w| (w.id, w.needs_you, w.state))
            .collect();
        assert!(counts.contains(&(1, 1, AgentState::NeedsYou)), "{counts:?}");
        assert!(counts.contains(&(2, 2, AgentState::NeedsYou)), "{counts:?}");
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
            nudge_pct: 80,
            hard_stop_pct: 95,
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
