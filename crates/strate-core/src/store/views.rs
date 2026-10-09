//! Read views for the app: workstream tabs, needs-you, per-workstream $
//! and the live graph. Each runs on a read-only connection from
//! [`super::Store::open_reader`] and serializes in camelCase for the
//! webview.
//!
//! A workstream draws one session: of the sessions with a segment in it, a
//! live one (a registry entry that is not gone) first, then the latest
//! segment start. Its live graph is that session's orchestrator plus the
//! session's subagents and teammates that joined one of the workstream's
//! segments (or none yet), with dispatch and teammate edges among them;
//! continuation edges and earlier sessions are never drawn.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::Result;

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
    /// Needs-you when anything in it waits on the user, else the drawn
    /// session's orchestrator state.
    pub state: AgentState,
    pub needs_you: i64,
    /// The session [`live_graph`] draws.
    pub session_id: Option<String>,
    /// Whether that session is listed by the registry.
    pub live: bool,
    /// The latest segment start in the workstream.
    pub started_at: Option<String>,
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
    /// The dispatching or team-lead agent, when it is drawn too.
    pub parent: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    /// dispatch | teammate.
    pub kind: String,
    pub parent: i64,
    pub child: i64,
}

/// Per workstream root: the session it draws, whether that one is live and
/// the workstream's latest segment start.
struct Drawn {
    session_id: String,
    live: bool,
    started_at: Option<String>,
}

/// The drawn session of every workstream root (or of the one `root`).
fn drawn(conn: &Connection, root: Option<i64>) -> Result<HashMap<i64, Drawn>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.root, g.session_id,
             EXISTS (SELECT 1 FROM registry AS x WHERE x.session_id = g.session_id AND x.gone = 0),
             max(g.started_at)
         FROM segments AS g JOIN workstream_roots AS r ON r.id = g.workstream_id
         WHERE ?1 IS NULL OR r.root = ?1
         GROUP BY r.root, g.session_id",
    )?;
    let rows = stmt.query_map([root], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Drawn {
                session_id: r.get(1)?,
                live: r.get(2)?,
                started_at: r.get(3)?,
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

/// A subagent's or teammate's state: hooks first, then whether its
/// dispatch was answered and whether its session is still live.
fn worker_state(needs_you: bool, hook: Option<&str>, answered: bool, live: bool) -> AgentState {
    match (needs_you, hook) {
        (true, _) => AgentState::NeedsYou,
        (_, Some("done")) => AgentState::Done,
        _ if answered || !live => AgentState::Done,
        (_, Some("idle")) => AgentState::Idle,
        _ => AgentState::Working,
    }
}

/// Every merge-resolved workstream, live first, then by latest start.
pub fn workstreams(conn: &Connection) -> Result<Vec<Workstream>> {
    let drawn = drawn(conn, None)?;
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
    let mut state = conn.prepare_cached(
        "SELECT registry_state FROM agents WHERE session_id = ?1 AND agent_id IS NULL",
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
        let registry_state: Option<String> = match shown {
            Some(d) => state
                .query_row([&d.session_id], |r| r.get(0))
                .optional()?
                .flatten(),
            None => None,
        };
        let needs_you = needs.get(&id).copied().unwrap_or(0);
        out.push(Workstream {
            id,
            name,
            fallback,
            cost_usd,
            unpriced,
            state: orchestrator_state(needs_you > 0, registry_state.as_deref()),
            needs_you,
            session_id: shown.map(|d| d.session_id.clone()),
            live: shown.is_some_and(|d| d.live),
            started_at: shown.and_then(|d| d.started_at.clone()),
        });
    }
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

/// Everything waiting on the user, oldest first.
pub fn needs_you(conn: &Connection) -> Result<Vec<NeedsYou>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.source, n.session_id, n.agent_id, n.waiting_for, n.since_ms, r.root
         FROM needs_you AS n
         LEFT JOIN sessions AS s ON s.session_id = n.session_id
         LEFT JOIN workstream_roots AS r ON r.id = s.workstream_id
         ORDER BY n.since_ms, n.session_id, n.agent_id",
    )?;
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

/// The graph of the session `workstream` draws (any id of a merge chain
/// resolves to its root); `None` when the workstream has no session.
pub fn live_graph(conn: &Connection, workstream: i64) -> Result<Option<LiveGraph>> {
    let root: Option<i64> = conn
        .prepare_cached("SELECT root FROM workstream_roots WHERE id = ?1")?
        .query_row([workstream], |r| r.get(0))
        .optional()?;
    let Some(root) = root else {
        return Ok(None);
    };
    let Some(shown) = drawn(conn, Some(root))?.remove(&root) else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.agent_id, a.kind, coalesce(a.registry_name, a.name), a.agent_type,
             a.description, a.model, a.cost_usd, a.context_tokens, a.context_pct,
             a.registry_state, a.hook_state,
             EXISTS (SELECT 1 FROM needs_you AS n WHERE n.agent = a.id),
             EXISTS (SELECT 1 FROM edges AS d
                 JOIN events AS e ON e.session_id = a.session_id AND e.tool_results IS NOT NULL
                 JOIN json_each(e.tool_results) AS j ON j.value = d.tool_use_id
                 WHERE d.child = a.id AND d.kind = 'dispatch'),
             (SELECT p.parent FROM edges AS p
                 WHERE p.child = a.id AND p.kind IN ('dispatch', 'teammate')),
             (SELECT p.kind FROM edges AS p
                 WHERE p.child = a.id AND p.kind IN ('dispatch', 'teammate'))
         FROM agents AS a
         LEFT JOIN segments AS g ON g.id = a.segment_id
         LEFT JOIN workstream_roots AS r ON r.id = g.workstream_id
         WHERE a.session_id = ?1 AND (a.agent_id IS NULL OR a.segment_id IS NULL OR r.root = ?2)
         ORDER BY a.agent_id IS NOT NULL, a.id",
    )?;
    let live = shown.live;
    let rows = stmt.query_map(rusqlite::params![shown.session_id, root], |r| {
        let agent_id: Option<String> = r.get(1)?;
        let needs: bool = r.get(12)?;
        let state = match agent_id {
            None => orchestrator_state(needs, r.get::<_, Option<String>>(10)?.as_deref()),
            Some(_) => worker_state(
                needs,
                r.get::<_, Option<String>>(11)?.as_deref(),
                r.get(13)?,
                live,
            ),
        };
        let edge: Option<(i64, String)> = r
            .get::<_, Option<i64>>(14)?
            .zip(r.get::<_, Option<String>>(15)?);
        Ok((
            GraphAgent {
                id: r.get(0)?,
                agent_id,
                kind: r.get(2)?,
                name: r.get(3)?,
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
    let rows: Vec<(GraphAgent, Option<(i64, String)>)> = rows.collect::<Result<_>>()?;
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
        session_id: shown.session_id,
        live,
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
            agents: vec![],
            edges: vec![],
        };
        let mut got = keys(&graph);
        got.sort();
        assert_eq!(got, ["agents", "edges", "live", "sessionId"]);
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
        });
        got.sort();
        assert_eq!(
            got,
            [
                "costUsd",
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
