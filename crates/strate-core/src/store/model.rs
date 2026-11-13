//! The generic agent model (#107): every agent has a lane, a role, a
//! charge and a parent.
//!
//! - Lane (view `agent_lanes`): `subagent` for a subagent or teammate; an
//!   orchestrator is a `background-session` when a background registry row
//!   names a session of its `/clear` chain, else `interactive`.
//! - Role (`agents.role`): the role its name parses to ([`crate::charge`]).
//! - Own charge (`agents.own_charge`, source `branch` or `name`): its git
//!   branch (not empty, `main`, `master` or `HEAD`) is the head of cached
//!   PRs; one PR (its repo the name charge's, else a repo its cwd names,
//!   else the newest, then the lowest number) gives its lowest closing
//!   issue. Else its name charge (`agents.name_charge`): the first of its
//!   own name, its chain's registry name (orchestrators) and its meta
//!   description that parses; a bare form only from an orchestrator's
//!   own title (`fix-step-2`, `unit-3`, a registry `code-42` are no
//!   issues).
//! - Parent (view `spawn_parents`): its dispatch, teammate, launch or
//!   message edge, in that order.
//!   - A background session's orchestrator hangs (`launch`, #108) under the
//!     agent whose Bash or PowerShell tool_use ran the `claude --bg`
//!     command that started it (table `launches`, [`launches`]): the
//!     session its `--resume` names, else the earliest link, started at or
//!     after it, of the `/clear` chain of the background registry row
//!     whose short id its result prints as a whole token, else the
//!     earliest background-lane orchestrator named as its `--name`
//!     (trimmed, case-folded; its own name, else its registry name) that
//!     started at or after it. Launches go in time order, those naming
//!     their child by resume or short id before those by name alone: each
//!     links at most one child and a child takes the first launch to
//!     reach it, refusing one whose parent descends from the child through
//!     a dispatch, teammate or earlier launch edge.
//!   - A session's orchestrator hangs (`message`) under its first
//!     messager: the agent whose record holds the msg_id of a SendMessage
//!     result that one of the session's records received, by receipt time,
//!     then msg_id, refusing one whose parent descends from the session
//!     through any spawn edge, and one whose parent's `/clear` chain (its
//!     process root's links) started after the session's chain (#133): a
//!     younger messager is a peer, never the spawner of what ran before it.
//! - Finished (a subagent's or teammate's, [`FINISHED_SQL`]): its meta.json
//!   `finished` (`agents.finished`, sticky), its task notification, or its
//!   dispatch answered (`agents.dispatch_result`; a background dispatch's
//!   `async_launched` launch result is no answer, and marks it
//!   background); else its lead's process gone (#115): no live registry
//!   row of its `/clear` chain started by its last record, and a poll since
//!   that record. A process ends once, so this is terminal too: a later
//!   process of the chain (a resume) started after the record, and an idle
//!   notice never counts, because an idle teammate can be woken.
//! - Effective charge (view `agent_charges`): its own, else its nearest
//!   spawn ancestor's own, else Unlinked.
//!
//! Every stored value is a function of stored rows, recomputed whenever a
//! batch, graph, registry poll or GitHub refresh changes an input, so an
//! incremental build equals a cold one. Meta `finished` is the exception:
//! it is read from disk at a discovery pass (not every scan). The meta is written after
//! the subagent's last record, so a batch on its transcript (which re-reads
//! it while unset) rarely sees it; its task notification or dispatch
//! answer usually finishes it first.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::Value;

use super::Result;
use crate::charge::{Charge, ChargeKind, NameForm, ParsedName, parse_agent_name};
use crate::discovery::launches;

/// What the stored model was built by, under meta `model`: a store at
/// another version (one from before v21 too) is rebuilt on open. Bump it
/// when resolution changes. v2 (#108) derives the launches and dispatch
/// results; v3 (#133) refuses a message edge from a younger chain.
const MODEL_VERSION: &str = "3";

/// Where an agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    /// A subagent or teammate.
    Subagent,
    /// An orchestrator of a `claude --bg` process.
    BackgroundSession,
    /// Any other orchestrator: a session someone typed into.
    Interactive,
}

/// Where an agent's effective charge came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeSource {
    /// Its git branch's PR's closing issue.
    Branch,
    /// Its name.
    Name,
    /// Its nearest spawn ancestor's own charge.
    Parent,
    /// None of those: no charge.
    Unlinked,
}

/// One agent of the model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAgent {
    /// The agent's row id; `parent` refers to it.
    pub id: i64,
    pub session_id: String,
    /// `None` on the orchestrator.
    pub agent_id: Option<String>,
    /// orchestrator | subagent | teammate.
    pub kind: String,
    pub lane: Lane,
    /// The role its name gives, if any.
    pub role: Option<String>,
    /// Its own name, else an orchestrator's registry name.
    pub name: Option<String>,
    /// The effective charge; `None` is Unlinked.
    pub charge: Option<Charge>,
    pub charge_source: ChargeSource,
    /// The spawn parent's row id.
    pub parent: Option<i64>,
    /// dispatch | teammate | launch | message.
    pub parent_via: Option<String>,
    /// A subagent or teammate has finished ([`FINISHED_SQL`]): its
    /// meta.json says so, its task notification arrived, or its dispatch
    /// was answered (a background dispatch's `async_launched` result is no
    /// answer); else the process that ran it is gone: no live registry row
    /// of its `/clear` chain started by its last record, and a poll since
    /// then (#115). Never while such a row is live, never on an idle notice
    /// alone, never for an orchestrator. `false` is not running: a worker
    /// with no record, or none since the registry's newest change, waits
    /// for the next poll, and a resting teammate (`views.rs`
    /// `session_agents`) is idle.
    pub finished: bool,
    /// A worker dispatched in the background: its dispatch's tool_result
    /// is `async_launched`.
    pub background: bool,
}

/// The registry name of orchestrator `a`: of the newest row (a live one
/// first) naming a session of its `/clear` chain (one process root) with a
/// name. The process's name is every link's.
const REGISTRY_NAME: &str = "(SELECT r.name FROM registry AS r
    JOIN sessions AS rs ON rs.session_id = r.session_id
    JOIN sessions AS s ON s.session_id = a.session_id
    WHERE r.name <> ''
        AND ifnull(rs.process_root, rs.session_id) = ifnull(s.process_root, s.session_id)
    ORDER BY r.gone, r.last_seen_ms DESC, r.id DESC LIMIT 1)";

/// Whether worker `a` (an `agents` row so aliased) has finished, the one
/// rule [`agent_model`] and `views.rs` `session_agents` read (see
/// [`ModelAgent::finished`]). The signals first; only a worker none of them
/// finished reads its last record (`w.last`, unix ms) for the registry
/// check: no live row of its `/clear` chain's process started by then, and
/// a poll at or after it (the newest row change) listed every running
/// process. No record, or no such poll yet, is not finished.
pub(super) const FINISHED_SQL: &str = "(a.agent_id IS NOT NULL AND CASE
    WHEN a.finished IS 1 OR a.dispatch_result IS 'answered'
        OR EXISTS (SELECT 1 FROM events AS n
            WHERE n.session_id = a.session_id AND n.notification_task = a.agent_id) THEN 1
    ELSE ifnull((SELECT w.last <= (SELECT max(p.last_seen_ms) FROM registry AS p)
            AND NOT EXISTS (SELECT 1 FROM registry AS r
                JOIN sessions AS rs ON rs.session_id = r.session_id
                JOIN sessions AS s ON s.session_id = a.session_id
                WHERE r.gone = 0 AND r.started_at_ms <= w.last
                    AND ifnull(rs.process_root, rs.session_id)
                        = ifnull(s.process_root, s.session_id))
        FROM (SELECT CAST(round(max(unixepoch(e.timestamp, 'subsec')) * 1000) AS INTEGER) AS last
            FROM events AS e
            WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id) AS w), 0)
    END)";

/// Every agent of the model, by session, orchestrator first.
pub fn agent_model(conn: &Connection) -> Result<Vec<ModelAgent>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id, a.session_id, a.agent_id, a.kind, l.lane, a.role,
             coalesce(a.name, CASE WHEN a.agent_id IS NULL THEN {REGISTRY_NAME} END),
             c.kind, c.repo, c.number, c.topic, x.source, p.parent, p.kind,
             {FINISHED_SQL},
             a.dispatch_result IS 'async_launched'
         FROM agents AS a
         JOIN agent_lanes AS l ON l.agent = a.id
         JOIN agent_charges AS x ON x.agent = a.id
         LEFT JOIN charges AS c ON c.id = x.charge
         LEFT JOIN spawn_parents AS p ON p.child = a.id
         ORDER BY a.session_id, a.agent_id IS NOT NULL, a.agent_id"
    ))?;
    stmt.query_map([], |r| {
        Ok(ModelAgent {
            id: r.get(0)?,
            session_id: r.get(1)?,
            agent_id: r.get(2)?,
            kind: r.get(3)?,
            lane: lane(&r.get::<_, String>(4)?),
            role: r.get(5)?,
            name: r.get(6)?,
            charge: charge_at(r, 7)?,
            charge_source: source(&r.get::<_, String>(11)?),
            parent: r.get(12)?,
            parent_via: r.get(13)?,
            finished: r.get(14)?,
            background: r.get(15)?,
        })
    })?
    .collect()
}

/// [`effective_charge`]'s query: `agent_charges` for agent `?1` alone,
/// walking up from it only (the view walks up from every agent).
const EFFECTIVE_CHARGE_SQL: &str = "WITH RECURSIVE up (at, depth, seen) AS (
         SELECT ?1, 0, ',' || ?1 || ','
         UNION ALL
         SELECT p.parent, up.depth + 1, up.seen || p.parent || ','
         FROM up
         JOIN agents AS a ON a.id = up.at AND a.own_charge IS NULL
         JOIN spawn_parents AS p ON p.child = up.at
         WHERE up.depth < 32 AND instr(up.seen, ',' || p.parent || ',') = 0
     )
     SELECT c.kind, c.repo, c.number, c.topic,
         CASE WHEN up.depth = 0 THEN h.own_charge_source ELSE 'parent' END
     FROM up JOIN agents AS h ON h.id = up.at JOIN charges AS c ON c.id = h.own_charge
     ORDER BY up.depth LIMIT 1";

/// Agent row `agent`'s effective charge and where it came from, as the
/// `agent_charges` view resolves it; `(None, Unlinked)` with none.
pub(super) fn effective_charge(
    conn: &Connection,
    agent: i64,
) -> Result<(Option<Charge>, ChargeSource)> {
    let found = conn
        .prepare_cached(EFFECTIVE_CHARGE_SQL)?
        .query_row([agent], |r| {
            Ok((charge_at(r, 0)?, source(&r.get::<_, String>(4)?)))
        })
        .optional()?;
    Ok(found.unwrap_or((None, ChargeSource::Unlinked)))
}

pub(super) fn lane(s: &str) -> Lane {
    match s {
        "subagent" => Lane::Subagent,
        "background-session" => Lane::BackgroundSession,
        _ => Lane::Interactive,
    }
}

fn source(s: &str) -> ChargeSource {
    match s {
        "branch" => ChargeSource::Branch,
        "name" => ChargeSource::Name,
        "parent" => ChargeSource::Parent,
        _ => ChargeSource::Unlinked,
    }
}

/// The charge in columns `at..at + 4` (kind, repo, number, topic).
fn charge_at(r: &Row, at: usize) -> Result<Option<Charge>> {
    let kind: Option<String> = r.get(at)?;
    let Some(kind) = kind.as_deref().and_then(ChargeKind::parse) else {
        return Ok(None);
    };
    Ok(Some(Charge {
        kind,
        repo: r.get(at + 1)?,
        number: r.get::<_, Option<i64>>(at + 2)?.map(|n| n as u64),
        topic: r.get(at + 3)?,
    }))
}

/// Rebuilds the model unless meta `model` says this build made it.
pub(super) fn refresh(conn: &Connection) -> Result<()> {
    let built: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'model'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if built.as_deref() == Some(MODEL_VERSION) {
        return Ok(());
    }
    rebuild(conn)?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('model', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [MODEL_VERSION],
    )?;
    Ok(())
}

/// Rebuilds every agent's role, own charge and dispatch result, the
/// launches from the stored events, and the launch and message edges.
pub(super) fn rebuild(conn: &Connection) -> Result<()> {
    let all: Vec<i64> = conn
        .prepare("SELECT id FROM agents ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    resolve(conn, all)?;
    dispatch_results(conn)?;
    derive_launches(conn)?;
    launch_edges(conn)?;
    messages(conn)
}

/// Each dispatched subagent and the first stored event holding its
/// dispatch's tool_result. CROSS JOIN keeps the events outermost: read
/// once, not once per edge.
const DISPATCH_RESULTS_SQL: &str = "SELECT d.child, min(e.id) AS event
    FROM events AS e CROSS JOIN json_each(e.tool_results) AS j
    CROSS JOIN edges AS d ON d.kind = 'dispatch' AND d.tool_use_id = j.value
    WHERE e.tool_results IS NOT NULL
    GROUP BY d.child";

/// Each stored launch's tool_use id and the first stored event holding its
/// tool_result, the events read once as in [`DISPATCH_RESULTS_SQL`].
const LAUNCH_RESULTS_SQL: &str = "SELECT l.tool_use_id, min(e.id) AS event
    FROM events AS e CROSS JOIN json_each(e.tool_results) AS j
    CROSS JOIN launches AS l ON l.tool_use_id = j.value
    WHERE e.tool_results IS NOT NULL
    GROUP BY l.tool_use_id";

/// Every subagent's dispatch result, from its dispatch's first stored
/// tool_result.
fn dispatch_results(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "UPDATE agents SET dispatch_result = NULL WHERE dispatch_result IS NOT NULL;
         WITH r AS ({DISPATCH_RESULTS_SQL})
         UPDATE agents SET dispatch_result = (
             SELECT CASE WHEN json_extract(x.raw, '$.toolUseResult.status') IS 'async_launched'
                 THEN 'async_launched' ELSE 'answered' END
             FROM events AS x WHERE x.id = r.event)
         FROM r WHERE agents.id = r.child;"
    ))
}

/// Re-derives the `launches` table from the stored Bash and PowerShell
/// tool_uses that mention a `--b…` flag.
fn derive_launches(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM launches", [])?;
    let records: Vec<(i64, String)> = conn
        .prepare(
            "SELECT a.id, e.raw FROM events AS e
             JOIN agents AS a ON a.session_id = e.session_id
                 AND ifnull(a.agent_id, '') = ifnull(e.agent_id, '')
             WHERE e.tool_uses IS NOT NULL
                 AND (e.tool_uses LIKE '%\"Bash\"]%' OR e.tool_uses LIKE '%\"PowerShell\"]%')
                 AND instr(e.raw, '--b') > 0
             ORDER BY e.id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let mut any = false;
    for (agent, raw) in records {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            any |= launch_record(conn, agent, &value, false)?;
        }
    }
    if !any {
        return Ok(());
    }
    // Every result at once, not a scan of the launcher's events per launch.
    let found: Vec<(String, i64)> = conn
        .prepare(LAUNCH_RESULTS_SQL)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    for (id, event) in found {
        let raw: String = conn
            .prepare_cached("SELECT raw FROM events WHERE id = ?1")?
            .query_row([event], |r| r.get(0))?;
        result_in(conn, &id, &raw)?;
    }
    Ok(())
}

/// After a batch of `session_id`: its transcript's agent (`agent`, when it
/// stored a record) and the orchestrators of its `/clear` chain, whose
/// registry name the batch may have changed by linking the session in.
pub(super) fn batch(conn: &Connection, session_id: &str, agent: Option<i64>) -> Result<()> {
    let mut ids = chain_orchestrators(conn, session_id)?;
    ids.extend(agent);
    resolve(conn, ids)
}

/// After a registry row naming `session_id` changed: its chain's
/// orchestrators, and the launch edges, which read background rows' short
/// ids, names and lanes.
pub(super) fn listed(conn: &Connection, session_id: &str) -> Result<()> {
    resolve(conn, chain_orchestrators(conn, session_id)?)?;
    if launch_edges(conn)? {
        messages(conn)?;
    }
    Ok(())
}

/// After a discovery graph: every agent of `sessions`, then the launch and
/// message edges, whose cycle checks walk the graph's dispatch and teammate
/// edges.
pub(super) fn graph<'a>(
    conn: &Connection,
    sessions: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut ids = Vec::new();
    let mut stmt = conn.prepare_cached("SELECT id FROM agents WHERE session_id = ?1")?;
    for session in sessions.into_iter().collect::<HashSet<_>>() {
        for id in stmt.query_map([session], |r| r.get(0))? {
            ids.push(id?);
        }
    }
    resolve(conn, ids)?;
    launch_edges(conn)?;
    messages(conn)
}

/// After a batch moved something a launch edge reads (a launch or its
/// result, an orchestrator's name, start or process root): rebuilds the
/// launch edges, then the message edges, which read every chain's start
/// (#133: an earlier link arriving ages its chain), whatever `_messaged`
/// (the batch stored a cross-session message) says.
pub(super) fn relaunch(conn: &Connection, _messaged: bool) -> Result<()> {
    launch_edges(conn)?;
    messages(conn)
}

/// After a GitHub refresh: every agent on a branch that can head a PR.
pub(super) fn branched(conn: &Connection) -> Result<()> {
    let ids: Vec<i64> = conn
        .prepare_cached(
            "SELECT id FROM agents
             WHERE git_branch IS NOT NULL AND git_branch NOT IN ('', 'main', 'master', 'HEAD')",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    resolve(conn, ids)
}

/// The orchestrators of `session_id`'s `/clear` chain (one process root).
fn chain_orchestrators(conn: &Connection, session_id: &str) -> Result<Vec<i64>> {
    conn.prepare_cached(
        "SELECT a.id FROM sessions AS s
         JOIN sessions AS l ON (l.session_id = ifnull(s.process_root, s.session_id)
                 OR l.process_root = ifnull(s.process_root, s.session_id))
             AND ifnull(l.process_root, l.session_id) = ifnull(s.process_root, s.session_id)
         JOIN agents AS a ON a.session_id = l.session_id AND a.agent_id IS NULL
         WHERE s.session_id = ?1",
    )?
    .query_map([session_id], |r| r.get(0))?
    .collect()
}

/// Recomputes the role, name charge and own charge of each of `agents`.
fn resolve(conn: &Connection, agents: impl IntoIterator<Item = i64>) -> Result<()> {
    for id in agents {
        let inputs = conn
            .prepare_cached(&format!(
                "SELECT a.name, CASE WHEN a.agent_id IS NULL THEN {REGISTRY_NAME} END,
                     a.description, a.git_branch, a.cwd, a.agent_id IS NULL
                 FROM agents AS a WHERE a.id = ?1"
            ))?
            .query_row([id], |r| {
                Ok(Inputs {
                    name: r.get(0)?,
                    registry_name: r.get(1)?,
                    description: r.get(2)?,
                    branch: r.get(3)?,
                    cwd: r.get(4)?,
                    orchestrator: r.get(5)?,
                })
            })
            .optional()?;
        let Some(inputs) = inputs else { continue };
        let named = inputs.parsed();
        let name_charge = named.as_ref().map(|p| &p.charge);
        let name_repo = name_charge.and_then(|c| c.repo.as_deref());
        let own = match branch_charge(conn, &inputs, name_repo)? {
            Some(c) => Some((c, "branch")),
            None => name_charge.map(|c| (c.clone(), "name")),
        };
        let name_id = name_charge.map(|c| charge_id(conn, c)).transpose()?;
        let own_id = own.as_ref().map(|(c, _)| charge_id(conn, c)).transpose()?;
        conn.prepare_cached(
            "UPDATE agents SET role = ?2, name_charge = ?3, own_charge = ?4, own_charge_source = ?5
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            named.and_then(|p| p.role),
            name_id,
            own_id,
            own.map(|(_, source)| source),
        ])?;
    }
    Ok(())
}

/// What an agent's role and own charge are read from.
struct Inputs {
    name: Option<String>,
    registry_name: Option<String>,
    description: Option<String>,
    branch: Option<String>,
    cwd: Option<String>,
    orchestrator: bool,
}

impl Inputs {
    /// The first of its name, registry name and description that parses,
    /// in a form other than bare unless an orchestrator's own name.
    fn parsed(&self) -> Option<ParsedName> {
        // A bare `<slug>-<N>` is an issue only as an orchestrator's title,
        // which the user set. lib.mjs `isChosenName` takes a bare name only
        // when user- or peer-set; strate has no registry nameSource, so a
        // bare registry name (`code-42`), a worker's meta name (`unit-3`,
        // `recon-2`) or a description of that shape is a label.
        let chosen = |p: &ParsedName, bare_ok: bool| bare_ok || p.form != NameForm::Bare;
        [
            (&self.name, self.orchestrator),
            (&self.registry_name, false),
            (&self.description, false),
        ]
        .into_iter()
        .find_map(|(n, bare_ok)| parse_agent_name(n.as_deref()?).filter(|p| chosen(p, bare_ok)))
    }
}

/// The issue the agent's branch leads to: of the cached PRs headed by it,
/// one in `name_repo`, else in a repo its cwd names (a path segment that is
/// the slug or starts `<slug>-`), else the newest, then the lowest number;
/// that PR's lowest closing issue.
fn branch_charge(
    conn: &Connection,
    inputs: &Inputs,
    name_repo: Option<&str>,
) -> Result<Option<Charge>> {
    let Some(branch) = inputs
        .branch
        .as_deref()
        .filter(|b| !matches!(*b, "" | "main" | "master" | "HEAD"))
    else {
        return Ok(None);
    };
    let prs: Vec<(String, i64, i64, String)> = conn
        .prepare_cached(
            "SELECT repo, number, min(issue), max(created_at) FROM gh_prs
             WHERE head_ref = ?1 GROUP BY repo, number",
        )?
        .query_map([branch], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_>>()?;
    let segments: Vec<String> = inputs
        .cwd
        .as_deref()
        .unwrap_or_default()
        .split(['/', '\\'])
        .map(str::to_ascii_lowercase)
        .collect();
    let rank = |repo: &str| {
        let repo = repo.to_ascii_lowercase();
        let prefix = format!("{repo}-");
        if name_repo.is_some_and(|n| n.eq_ignore_ascii_case(&repo)) {
            0
        } else if segments
            .iter()
            .any(|s| *s == repo || s.starts_with(&prefix))
        {
            1
        } else {
            2
        }
    };
    let best = prs.into_iter().min_by(|a, b| {
        (rank(&a.0).cmp(&rank(&b.0)))
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(best.map(|(repo, _, issue, _)| Charge {
        kind: ChargeKind::Issue,
        repo: Some(repo),
        number: Some(issue as u64),
        topic: None,
    }))
}

/// The `charges` row of `c`, made when missing.
fn charge_id(conn: &Connection, c: &Charge) -> Result<i64> {
    conn.prepare_cached(
        "INSERT INTO charges (key, kind, repo, number, topic) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (key) DO UPDATE SET kind = excluded.kind
         RETURNING id",
    )?
    .query_row(
        params![
            c.key(),
            c.kind.as_str(),
            c.repo,
            c.number.map(|n| n as i64),
            c.topic
        ],
        |r| r.get(0),
    )
}

/// Rebuilds the message edges from the stored events: each session's
/// orchestrator under its first messager (see the module doc), a
/// candidate whose parent it is an ancestor of, or whose parent's `/clear`
/// chain started after the session's (a peer, #133), refused for the next.
/// A chain with no known start, or two chains that started together, keeps
/// the edge: only a strictly younger messager is a peer.
pub(super) fn messages(conn: &Connection) -> Result<()> {
    let mut up = spawn_up(conn, true)?;
    let candidates: Vec<(i64, i64)> = conn
        .prepare_cached(
            "WITH chains AS (SELECT ifnull(process_root, session_id) AS root,
                     min(unixepoch(started_at, 'subsec')) AS start
                 FROM sessions GROUP BY 1)
             SELECT p.id, c.id
             FROM events AS r
             JOIN events AS s ON s.sent_msg_id = r.peer_msg_id AND s.session_id <> r.session_id
             JOIN agents AS p ON p.session_id = s.session_id
                 AND ifnull(p.agent_id, '') = ifnull(s.agent_id, '')
             JOIN agents AS c ON c.session_id = r.session_id AND ifnull(c.agent_id, '') = ''
             JOIN sessions AS ps ON ps.session_id = s.session_id
             JOIN sessions AS cs ON cs.session_id = r.session_id
             JOIN chains AS pc ON pc.root = ifnull(ps.process_root, ps.session_id)
             JOIN chains AS cc ON cc.root = ifnull(cs.process_root, cs.session_id)
             WHERE r.peer_msg_id IS NOT NULL AND NOT ifnull(pc.start > cc.start, 0)
             ORDER BY r.timestamp IS NULL, r.timestamp, r.peer_msg_id, s.session_id,
                 s.agent_id IS NOT NULL, s.agent_id, r.session_id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let mut chosen: BTreeMap<i64, i64> = BTreeMap::new();
    for (parent, child) in candidates {
        if chosen.contains_key(&child) || descends(parent, child, &up) {
            continue;
        }
        chosen.insert(child, parent);
        up.entry(child).or_default().push(parent);
    }
    conn.execute("DELETE FROM edges WHERE kind = 'message'", [])?;
    let mut insert =
        conn.prepare_cached("INSERT INTO edges (kind, parent, child) VALUES ('message', ?1, ?2)")?;
    for (child, parent) in chosen {
        insert.execute([parent, child])?;
    }
    Ok(())
}

/// Each child's parents by its dispatch and teammate edges, and its launch
/// edges with `launch`.
fn spawn_up(conn: &Connection, launch: bool) -> Result<HashMap<i64, Vec<i64>>> {
    let mut up: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut spawns = conn.prepare_cached(
        "SELECT child, parent FROM edges
         WHERE (kind IN ('dispatch', 'teammate') OR ?1 AND kind = 'launch')
             AND parent IS NOT NULL",
    )?;
    for edge in spawns.query_map([launch], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?)))? {
        let (child, parent) = edge?;
        up.entry(child).or_default().push(parent);
    }
    Ok(up)
}

/// Stores the `claude --bg` launches in record `value` of agent row
/// `agent` (a Bash or PowerShell tool_use, each with the tool_result
/// already stored when `stored`, if any), and the launch result it
/// carries; whether it stored either.
pub(super) fn launch_record(
    conn: &Connection,
    agent: i64,
    value: &Value,
    stored: bool,
) -> Result<bool> {
    let blocks = value["message"]["content"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut changed = false;
    for block in blocks {
        match block["type"].as_str() {
            Some("tool_use") if matches!(block["name"].as_str(), Some("Bash" | "PowerShell")) => {
                let (Some(id), Some(command)) =
                    (block["id"].as_str(), block["input"]["command"].as_str())
                else {
                    continue;
                };
                let found = launches(command);
                for (seq, launch) in found.iter().enumerate() {
                    changed |=
                        conn.prepare_cached(
                            "INSERT OR IGNORE INTO launches
                                 (agent, tool_use_id, seq, at, name, resume_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        )?
                        .execute(params![
                            agent,
                            id,
                            seq as i64,
                            value["timestamp"].as_str(),
                            launch.name,
                            launch.resume
                        ])? > 0;
                }
                if stored && !found.is_empty() {
                    stored_result(conn, agent, id)?;
                }
            }
            Some("tool_result") => {
                let Some(id) = block["tool_use_id"].as_str() else {
                    continue;
                };
                let pending: bool = conn
                    .prepare_cached(
                        "SELECT EXISTS (SELECT 1 FROM launches
                             WHERE tool_use_id = ?1 AND result IS NULL)",
                    )?
                    .query_row([id], |r| r.get(0))?;
                if pending {
                    set_result(conn, id, &result_text(value, block))?;
                    changed = true;
                }
            }
            _ => {}
        }
    }
    Ok(changed)
}

/// Fills launch `id`'s result from a tool_result agent row `agent`'s
/// transcript already holds.
fn stored_result(conn: &Connection, agent: i64, id: &str) -> Result<()> {
    let raw: Option<String> = conn
        .prepare_cached(
            "SELECT e.raw FROM agents AS a
             JOIN events AS e ON e.session_id = a.session_id AND e.agent_id IS a.agent_id
             WHERE a.id = ?1 AND e.tool_results IS NOT NULL AND instr(e.tool_results, ?2) > 0
             ORDER BY e.id LIMIT 1",
        )?
        .query_row(params![agent, format!("\"{id}\"")], |r| r.get(0))
        .optional()?;
    match raw {
        Some(raw) => result_in(conn, id, &raw),
        None => Ok(()),
    }
}

/// Fills launch `id`'s result from its tool_result in record `raw`.
fn result_in(conn: &Connection, id: &str, raw: &str) -> Result<()> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Ok(());
    };
    let blocks = value["message"]["content"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if let Some(block) = blocks.iter().find(|b| b["tool_use_id"] == id) {
        set_result(conn, id, &result_text(&value, block))?;
    }
    Ok(())
}

fn set_result(conn: &Connection, id: &str, text: &str) -> Result<()> {
    conn.prepare_cached(
        "UPDATE launches SET result = ?2 WHERE tool_use_id = ?1 AND result IS NULL",
    )?
    .execute(params![id, text])?;
    Ok(())
}

/// A tool_result's text: its content (a string, or its text parts), else
/// the record's `toolUseResult.stdout`.
fn result_text(value: &Value, block: &Value) -> String {
    let content = match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if content.is_empty() {
        value["toolUseResult"]["stdout"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    } else {
        content
    }
}

/// One stored launch, as the launch edges read it.
struct Launched {
    parent: i64,
    tool_use_id: String,
    /// Its record's time, in unix seconds.
    at: Option<f64>,
    name: Option<String>,
    resume: Option<String>,
    result: Option<String>,
}

/// The background-lane orchestrators, read once per [`launch_edges`].
#[derive(Default)]
struct Background {
    /// Each `/clear` chain's orchestrators by process root, by start.
    chains: HashMap<String, Vec<Link>>,
    /// The orchestrators by trimmed, case-folded name (their own, else
    /// their chain's registry name), by start, then session.
    named: HashMap<String, Vec<Link>>,
    /// Each background registry row's short id, and its process root.
    short_ids: HashMap<String, String>,
}

/// An orchestrator of [`Background`] and its start, in unix seconds.
#[derive(Clone, Copy)]
struct Link {
    id: i64,
    start: Option<f64>,
}

impl Background {
    fn load(conn: &Connection) -> Result<Self> {
        let mut registry_names: HashMap<String, String> = HashMap::new();
        let mut stmt = conn.prepare_cached(
            "SELECT ifnull(rs.process_root, rs.session_id), r.name FROM registry AS r
             JOIN sessions AS rs ON rs.session_id = r.session_id
             WHERE r.name <> ''
             ORDER BY r.gone, r.last_seen_ms DESC, r.id DESC",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))? {
            let (root, name) = row?;
            registry_names.entry(root).or_insert(name);
        }
        let mut out = Self::default();
        let mut stmt = conn.prepare_cached(
            "WITH roots AS (SELECT DISTINCT ifnull(rs.process_root, rs.session_id) AS root
                 FROM registry AS r JOIN sessions AS rs ON rs.session_id = r.session_id
                 WHERE r.kind = 'background')
             SELECT a.id, roots.root, unixepoch(s.started_at, 'subsec'), a.name
             FROM roots
             JOIN sessions AS s ON (s.session_id = roots.root OR s.process_root = roots.root)
                 AND ifnull(s.process_root, s.session_id) = roots.root
             JOIN agents AS a ON a.session_id = s.session_id AND a.agent_id IS NULL
             ORDER BY unixepoch(s.started_at, 'subsec') IS NULL,
                 unixepoch(s.started_at, 'subsec'), s.session_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                Link {
                    id: r.get(0)?,
                    start: r.get(2)?,
                },
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (link, root, name) = row?;
            let name = name.or_else(|| registry_names.get(&root).cloned());
            if let Some(name) = name
                .map(|n| n.trim().to_lowercase())
                .filter(|n| !n.is_empty())
            {
                out.named.entry(name).or_default().push(link);
            }
            out.chains.entry(root).or_default().push(link);
        }
        let mut stmt = conn.prepare_cached(
            "SELECT r.bg_id, ifnull(rs.process_root, rs.session_id) FROM registry AS r
             JOIN sessions AS rs ON rs.session_id = r.session_id
             WHERE r.kind = 'background' AND r.bg_id <> ''
             ORDER BY r.id",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))? {
            let (bg_id, root) = row?;
            out.short_ids.entry(bg_id).or_insert(root);
        }
        Ok(out)
    }
}

/// Whether a link started at `start` started at or after `at`; with no
/// launch time, any link does.
fn since(start: Option<f64>, at: Option<f64>) -> bool {
    match (start, at) {
        (Some(start), Some(at)) => start >= at,
        (_, None) => true,
        (None, Some(_)) => false,
    }
}

/// Rebuilds the launch edges from the stored launches (see the module
/// doc); whether they changed.
fn launch_edges(conn: &Connection) -> Result<bool> {
    let before: BTreeMap<i64, (i64, String)> = conn
        .prepare_cached("SELECT child, parent, tool_use_id FROM edges WHERE kind = 'launch'")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<Result<_>>()?;
    let rows: Vec<Launched> = conn
        .prepare_cached(
            "SELECT agent, tool_use_id, unixepoch(at, 'subsec'), name, resume_id, result
             FROM launches
             ORDER BY unixepoch(at, 'subsec') IS NULL, unixepoch(at, 'subsec'), tool_use_id, seq",
        )?
        .query_map([], |r| {
            Ok(Launched {
                parent: r.get(0)?,
                tool_use_id: r.get(1)?,
                at: r.get(2)?,
                name: r.get(3)?,
                resume: r.get(4)?,
                result: r.get(5)?,
            })
        })?
        .collect::<Result<_>>()?;
    if rows.is_empty() && before.is_empty() {
        return Ok(false);
    }
    let background = Background::load(conn)?;
    let mut up = spawn_up(conn, false)?;
    let mut chosen: BTreeMap<i64, (i64, String)> = BTreeMap::new();
    let mut take = |launch: &Launched, candidates: Vec<i64>| {
        let child = candidates
            .into_iter()
            .find(|c| !chosen.contains_key(c) && !descends(launch.parent, *c, &up));
        if let Some(child) = child {
            up.entry(child).or_default().push(launch.parent);
            chosen.insert(child, (launch.parent, launch.tool_use_id.clone()));
        }
    };
    // A launch naming its child outright goes before any by name alone.
    let mut by_name = Vec::new();
    for launch in &rows {
        match named_outright(conn, launch, &background)? {
            Some(candidates) => take(launch, candidates),
            None => by_name.push(launch),
        }
    }
    for launch in by_name {
        take(launch, named_alike(launch, &background));
    }
    if chosen == before {
        return Ok(false);
    }
    conn.execute("DELETE FROM edges WHERE kind = 'launch'", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO edges (kind, parent, child, tool_use_id) VALUES ('launch', ?1, ?2, ?3)",
    )?;
    for (child, (parent, tool_use_id)) in chosen {
        insert.execute(params![parent, child, tool_use_id])?;
    }
    Ok(true)
}

/// The orchestrators `launch` names outright, by the first rule that names
/// any: its `--resume` session, else for each background row whose short
/// id its result prints (in print order), the earliest link of that row's
/// `/clear` chain that started at or after it. `None` when neither names
/// any.
fn named_outright(
    conn: &Connection,
    launch: &Launched,
    background: &Background,
) -> Result<Option<Vec<i64>>> {
    if let Some(resume) = &launch.resume {
        let resumed: Option<i64> = conn
            .prepare_cached("SELECT id FROM agents WHERE session_id = ?1 AND agent_id IS NULL")?
            .query_row([resume], |r| r.get(0))
            .optional()?;
        if let Some(id) = resumed {
            return Ok(Some(vec![id]));
        }
    }
    let printed: Vec<i64> = launch
        .result
        .as_deref()
        .unwrap_or_default()
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter_map(|token| {
            let chain = background.chains.get(background.short_ids.get(token)?)?;
            chain
                .iter()
                .find(|l| since(l.start, launch.at))
                .map(|l| l.id)
        })
        .collect();
    Ok((!printed.is_empty()).then_some(printed))
}

/// The background-lane orchestrators named as `launch`'s `--name` that
/// started at or after it, earliest first.
fn named_alike(launch: &Launched, background: &Background) -> Vec<i64> {
    let (Some(name), Some(at)) = (&launch.name, launch.at) else {
        return Vec::new();
    };
    background
        .named
        .get(&name.trim().to_lowercase())
        .into_iter()
        .flatten()
        .filter(|l| l.start.is_some_and(|start| start >= at))
        .map(|l| l.id)
        .collect()
}

/// Whether `agent` is `ancestor` or reaches it walking up `up`.
fn descends(agent: i64, ancestor: i64, up: &HashMap<i64, Vec<i64>>) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![agent];
    while let Some(at) = stack.pop() {
        if at == ancestor {
            return true;
        }
        if seen.insert(at) {
            stack.extend(up.get(&at).into_iter().flatten());
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, query_plan};
    use crate::tail::test_dir;

    /// `events` rows carry their whole record inline, ahead of
    /// `tool_results`: the rebuild reads every event once, each tool_result
    /// finding its dispatch by index. Driven from the dispatch edges, it
    /// read every event once per edge: 823 s to open a real v20 store
    /// (#108).
    #[test]
    fn the_dispatch_and_launch_results_rebuild_reads_the_events_once() {
        let dir = test_dir("model-dispatch-plan");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        assert_eq!(
            query_plan(&conn, DISPATCH_RESULTS_SQL),
            [
                "SCAN e",
                "SCAN j VIRTUAL TABLE INDEX 1:",
                "SEARCH d USING INDEX edges_tool_use (tool_use_id=?)",
                "USE TEMP B-TREE FOR GROUP BY",
            ]
        );
        assert_eq!(
            query_plan(&conn, LAUNCH_RESULTS_SQL),
            [
                "SCAN e",
                "SCAN j VIRTUAL TABLE INDEX 1:",
                "SEARCH l USING COVERING INDEX sqlite_autoindex_launches_1 (tool_use_id=?)",
                "USE TEMP B-TREE FOR GROUP BY",
            ]
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_walk_up_finds_an_ancestor_through_any_parent_and_ends_on_a_cycle() {
        let up = HashMap::from([(1, vec![2]), (2, vec![3, 4]), (4, vec![1])]);
        assert!(descends(1, 3, &up));
        assert!(descends(1, 4, &up));
        assert!(descends(5, 5, &up), "an agent is its own ancestor");
        assert!(!descends(1, 9, &up), "the 1 -> 2 -> 4 -> 1 loop ends");
        assert!(!descends(3, 1, &up));
    }
}
