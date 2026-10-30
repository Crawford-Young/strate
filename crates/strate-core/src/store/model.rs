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
//! - Parent (view `spawn_parents`): its dispatch, teammate or message edge.
//!   A session's orchestrator hangs (`message`) under its first messager:
//!   the agent whose record holds the msg_id of a SendMessage result that
//!   one of the session's records received, by receipt time, then msg_id,
//!   refusing one whose parent descends from the session through any spawn
//!   edge.
//! - Effective charge (view `agent_charges`): its own, else its nearest
//!   spawn ancestor's own, else Unlinked.
//!
//! Every stored value is a function of stored rows, recomputed whenever a
//! batch, graph, registry poll or GitHub refresh changes an input, so an
//! incremental build equals a cold one.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use super::Result;
use crate::charge::{Charge, ChargeKind, NameForm, ParsedName, parse_agent_name};

/// What the stored model was built by, under meta `model`: a store at
/// another version (one from before v21 too) is rebuilt on open. Bump it
/// when resolution changes.
const MODEL_VERSION: &str = "1";

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
    /// dispatch | teammate | message.
    pub parent_via: Option<String>,
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

/// Every agent of the model, by session, orchestrator first.
pub fn agent_model(conn: &Connection) -> Result<Vec<ModelAgent>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id, a.session_id, a.agent_id, a.kind, l.lane, a.role,
             coalesce(a.name, CASE WHEN a.agent_id IS NULL THEN {REGISTRY_NAME} END),
             c.kind, c.repo, c.number, c.topic, x.source, p.parent, p.kind
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
        })
    })?
    .collect()
}

fn lane(s: &str) -> Lane {
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

/// Rebuilds every agent's role and own charge, and the message edges.
pub(super) fn rebuild(conn: &Connection) -> Result<()> {
    let all: Vec<i64> = conn
        .prepare("SELECT id FROM agents ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    resolve(conn, all)?;
    messages(conn)
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
/// orchestrators.
pub(super) fn listed(conn: &Connection, session_id: &str) -> Result<()> {
    resolve(conn, chain_orchestrators(conn, session_id)?)
}

/// After a discovery graph: every agent of `sessions`, then the message
/// edges, whose cycle check walks the graph's dispatch and teammate edges.
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
/// candidate whose parent it is an ancestor of refused for the next.
pub(super) fn messages(conn: &Connection) -> Result<()> {
    let mut up: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut spawns = conn.prepare_cached(
        "SELECT child, parent FROM edges
         WHERE kind IN ('dispatch', 'teammate') AND parent IS NOT NULL",
    )?;
    for edge in spawns.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?)))? {
        let (child, parent) = edge?;
        up.entry(child).or_default().push(parent);
    }
    let candidates: Vec<(i64, i64)> = conn
        .prepare_cached(
            "SELECT p.id, c.id
             FROM events AS r
             JOIN events AS s ON s.sent_msg_id = r.peer_msg_id AND s.session_id <> r.session_id
             JOIN agents AS p ON p.session_id = s.session_id
                 AND ifnull(p.agent_id, '') = ifnull(s.agent_id, '')
             JOIN agents AS c ON c.session_id = r.session_id AND ifnull(c.agent_id, '') = ''
             WHERE r.peer_msg_id IS NOT NULL
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
