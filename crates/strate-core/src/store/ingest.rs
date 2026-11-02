//! Writes tail batches and discovery graphs into the schema. Every function
//! runs inside the caller's transaction.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::{Result, account, live, model};
use crate::cost::SYNTHETIC;
use crate::discovery::{AgentRef, Graph, Link, Unresolved, meta_finished, meta_of, scalar_text};
use crate::tail::{Batch, TailEvent};

/// The agent a transcript belongs to, read from where it sits in the
/// layout: `<project>/<sessionId>.jsonl` is the orchestrator,
/// `<project>/<sessionId>/subagents/agent-<agentId>.jsonl` a subagent.
struct Owner {
    session_id: String,
    agent_id: Option<String>,
    project_dir: Option<String>,
}

fn name_of(path: Option<&Path>) -> Option<String> {
    path.and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
}

fn text_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn owner_of(path: &Path) -> Owner {
    let file = name_of(Some(path)).unwrap_or_default();
    let parent = path.parent();
    if name_of(parent).as_deref() == Some("subagents") {
        let session_dir = parent.and_then(Path::parent);
        let agent = file
            .strip_prefix("agent-")
            .and_then(|rest| rest.strip_suffix(".jsonl"));
        if let (Some(agent), Some(session_id)) = (agent, name_of(session_dir)) {
            return Owner {
                session_id,
                agent_id: Some(agent.to_string()),
                project_dir: name_of(session_dir.and_then(Path::parent)),
            };
        }
    }
    Owner {
        session_id: file.strip_suffix(".jsonl").unwrap_or(&file).to_string(),
        agent_id: None,
        project_dir: name_of(parent),
    }
}

/// Stores a batch's events and its checkpoint, then relinks the batch's
/// session ([`relink`]), ends the waits left open on its `/clear` chain's
/// earlier links, re-reads a subagent's meta `finished` until it is set,
/// re-resolves the agents it touched in the model, recomputes the rollups
/// it touches, and rebuilds the launch edges (when it stored a launch or
/// its result, or changed what [`launch_inputs`] reads) and the message
/// edges (when those changed, or it stored a cross-session message).
pub(super) fn batch(conn: &Connection, batch: &Batch) -> Result<()> {
    let owner = owner_of(&batch.path);
    let file = text_of(&batch.path);
    let mut agent = None;
    let mut requests = BTreeSet::new();
    let mut messaged = false;
    let mut launched = false;
    let inputs = launch_inputs(conn, &owner.session_id)?;
    for event in &batch.events {
        match event {
            TailEvent::Record { offset, value, .. } => {
                let id = match agent {
                    Some(id) => id,
                    None => *agent.insert(ensure_owner(conn, &owner, &file)?),
                };
                let stored = record(conn, &owner, id, &file, *offset, value)?;
                requests.extend(stored.request);
                messaged |= stored.message;
                launched |= stored.launch;
            }
            // The file was rewritten: its uuid-less events would block the
            // new content at the same offsets. Events with a uuid stay and
            // dedupe against the re-read.
            TailEvent::Reset { .. } => {
                conn.prepare_cached("DELETE FROM events WHERE file = ?1 AND uuid IS NULL")?
                    .execute([&file])?;
            }
            TailEvent::Malformed { .. } | TailEvent::Error { .. } | TailEvent::CaughtUp => {}
        }
    }
    relink(conn, &owner.session_id)?;
    if agent.is_some() {
        end_cleared_waits(conn, &owner.session_id)?;
    }
    // A linked subagent's (not a teammate's) records start no discovery pass, so its batch
    // reads the meta's `finished` too (#108). The meta is mostly written
    // after its last record, so a later pass usually reads it first.
    if let (Some(id), Some(agent_id)) = (agent, owner.agent_id.as_deref()) {
        let (finished, subagent): (bool, bool) = conn
            .prepare_cached("SELECT finished IS 1, kind = 'subagent' FROM agents WHERE id = ?1")?
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if subagent && !finished {
            set_finished(conn, id, meta_finished(&batch.path, agent_id))?;
        }
        if subagent {
            link_from_meta(conn, id, &owner.session_id, &batch.path, agent_id)?;
        }
    }
    model::batch(conn, &owner.session_id, agent)?;
    let touched = account::touched(conn, &owner.session_id, &requests)?;
    account::rollup(conn, touched.iter().map(String::as_str))?;
    if launched || launch_inputs(conn, &owner.session_id)? != inputs {
        model::relaunch(conn, messaged)?;
    } else if messaged {
        model::messages(conn)?;
    }
    if let Some(c) = batch.checkpoint {
        conn.prepare_cached(
            "INSERT INTO offsets (path, byte_offset, volume, file_index) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (path) DO UPDATE SET byte_offset = excluded.byte_offset,
                 volume = excluded.volume, file_index = excluded.file_index",
        )?
        .execute(params![
            file,
            c.offset as i64,
            c.identity.volume as i64,
            c.identity.index.to_string()
        ])?;
    }
    Ok(())
}

/// Stores subagent `id`'s dispatch edge while none links it yet, by
/// discovery's rule over the store instead of the files: the meta Claude
/// Code writes at spawn (before the first record) names a `toolUseId`,
/// and the stored agent of this session whose record holds that tool_use
/// dispatched it, else the stored sibling its `parentAgentId` names. The
/// discovery pass that links a new subagent re-reads every transcript
/// (4 s release on a real 2.3 GB home, ~20 s seen live in a debug build),
/// so a short-lived worker was drawn and gone with no edge (#109). A
/// teammate (`teamName`) and a worker with no meta are left to discovery.
fn link_from_meta(
    conn: &Connection,
    id: i64,
    session_id: &str,
    transcript: &Path,
    agent_id: &str,
) -> Result<()> {
    let linked: bool = conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM edges
                 WHERE child = ?1 AND kind IN ('dispatch', 'teammate'))",
        )?
        .query_row([id], |r| r.get(0))?;
    let Some(meta) = (!linked).then(|| meta_of(transcript, agent_id)).flatten() else {
        return Ok(());
    };
    if meta.team_name.is_some() {
        return Ok(());
    }
    let dispatcher = match &meta.tool_use_id {
        Some(tool_use_id) => conn
            .prepare_cached(
                "SELECT a.id FROM events AS e
                 JOIN agents AS a ON a.session_id = e.session_id AND a.agent_id IS e.agent_id
                 WHERE e.session_id = ?1 AND e.tool_uses IS NOT NULL
                     AND instr(e.tool_uses, ?2) > 0 AND a.id <> ?3
                 ORDER BY e.id LIMIT 1",
            )?
            .query_row(params![session_id, format!("\"{tool_use_id}\""), id], |r| {
                r.get(0)
            })
            .optional()?,
        None => None,
    };
    let sibling = match meta.parent_agent_id.as_deref().filter(|p| *p != agent_id) {
        Some(parent) => agent_row(conn, session_id, Some(parent))?,
        None => None,
    };
    let Some(parent) = dispatcher.or(sibling) else {
        return Ok(());
    };
    let tool_use_id = dispatcher.and(meta.tool_use_id.as_deref());
    edge(conn, "dispatch", Some(parent), id, tool_use_id, None, None)
}

/// Sets `session_id`'s start, its orchestrator's earliest record time,
/// then links the orchestrators of every session on its process root,
/// predecessor to successor in start order (`continuation` edges). Both are
/// functions of the stored events.
fn relink(conn: &Connection, session_id: &str) -> Result<()> {
    conn.prepare_cached(
        "UPDATE sessions SET started_at = (SELECT min(e.timestamp) FROM events AS e
             WHERE e.session_id = ?1 AND e.agent_id IS NULL)
         WHERE session_id = ?1",
    )?
    .execute([session_id])?;
    let chain: Vec<i64> = conn
        .prepare_cached(
            "SELECT a.id FROM sessions AS s
             JOIN sessions AS me ON me.process_root = s.process_root
             JOIN agents AS a ON a.session_id = s.session_id AND a.agent_id IS NULL
             WHERE me.session_id = ?1
             ORDER BY s.started_at IS NULL, s.started_at, s.session_id",
        )?
        .query_map([session_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    if let Some(first) = chain.first() {
        conn.prepare_cached("DELETE FROM edges WHERE kind = 'continuation' AND child = ?1")?
            .execute([first])?;
    }
    for pair in chain.windows(2) {
        conn.prepare_cached(
            "INSERT INTO edges (kind, parent, child) VALUES ('continuation', ?1, ?2)
             ON CONFLICT (kind, child) DO UPDATE SET parent = excluded.parent",
        )?
        .execute(params![pair[0], pair[1]])?;
    }
    Ok(())
}

/// What a launch edge reads of `session_id`'s orchestrator: its row, name,
/// start and process root. A batch on the session changes nothing else
/// they read, so one leaving these alone keeps the launch edges.
type LaunchInputs = Option<(i64, Option<String>, Option<String>, Option<String>)>;

fn launch_inputs(conn: &Connection, session_id: &str) -> Result<LaunchInputs> {
    conn.prepare_cached(
        "SELECT a.id, a.name, s.started_at, s.process_root FROM sessions AS s
         JOIN agents AS a ON a.session_id = s.session_id AND a.agent_id IS NULL
         WHERE s.session_id = ?1",
    )?
    .query_row([session_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()
}

/// [`relink`]s every stored session: a v1 store's upgrade.
pub(super) fn relink_all(conn: &Connection) -> Result<()> {
    let ids: Vec<String> = conn
        .prepare("SELECT session_id FROM sessions ORDER BY session_id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    for id in &ids {
        relink(conn, id)?;
    }
    Ok(())
}

/// A `/clear` takes the prompts on screen with it: each agent still
/// waiting on an earlier link of `session_id`'s chain stops waiting at the
/// next link's first record (its start). A request received after that
/// start was not on screen at the `/clear`, so it is left open.
fn end_cleared_waits(conn: &Connection, session_id: &str) -> Result<()> {
    let cleared: Vec<(i64, i64)> = conn
        .prepare_cached(
            "WITH me AS (SELECT ifnull(process_root, session_id) AS root FROM sessions
                 WHERE session_id = ?1),
             links AS (SELECT l.session_id, unixepoch(l.started_at, 'subsec') * 1000 AS start_ms
                 FROM me JOIN sessions AS l ON l.session_id = me.root OR l.process_root = me.root
                 WHERE l.started_at IS NOT NULL)
             SELECT a.id, CAST(round(min(n.start_ms)) AS INTEGER)
             FROM links AS l JOIN links AS n ON n.start_ms > l.start_ms
             JOIN agents AS a ON a.session_id = l.session_id AND a.hook_state = 'needs_you'
             GROUP BY a.id HAVING min(n.start_ms) > a.hook_at_ms",
        )?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    for (agent, end_ms) in cleared {
        conn.prepare_cached(
            "UPDATE agents SET hook_waiting_for = NULL,
                 hook_state = CASE WHEN agent_id IS NULL THEN NULL ELSE 'running' END
             WHERE id = ?1",
        )?
        .execute([agent])?;
        live::end_wait(conn, agent, end_ms)?;
    }
    Ok(())
}

/// Ensures the session, its orchestrator and (for a subagent file) the
/// subagent exist; returns the owning agent's row id.
fn ensure_owner(conn: &Connection, owner: &Owner, file: &str) -> Result<i64> {
    let session_file = owner.agent_id.is_none().then_some(file);
    upsert_session(
        conn,
        &owner.session_id,
        owner.project_dir.as_deref(),
        session_file,
    )?;
    let orchestrator = ensure_agent(conn, &owner.session_id, None, "orchestrator", session_file)?;
    match &owner.agent_id {
        None => Ok(orchestrator),
        Some(agent_id) => ensure_agent(
            conn,
            &owner.session_id,
            Some(agent_id),
            "subagent",
            Some(file),
        ),
    }
}

/// A session is a stub until its own transcript (`path`) has been seen.
pub(super) fn upsert_session(
    conn: &Connection,
    session_id: &str,
    project_dir: Option<&str>,
    path: Option<&str>,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO sessions (session_id, project_dir, path, stub) VALUES (?1, ?2, ?3, ?3 IS NULL)
         ON CONFLICT (session_id) DO UPDATE SET
             project_dir = coalesce(sessions.project_dir, excluded.project_dir),
             path = coalesce(excluded.path, sessions.path),
             stub = min(sessions.stub, excluded.stub)",
    )?
    .execute(params![session_id, project_dir, path])?;
    Ok(())
}

pub(super) fn ensure_agent(
    conn: &Connection,
    session_id: &str,
    agent_id: Option<&str>,
    kind: &str,
    path: Option<&str>,
) -> Result<i64> {
    conn.prepare_cached(
        "INSERT INTO agents (session_id, agent_id, kind, path) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (session_id, ifnull(agent_id, '')) DO UPDATE SET
             path = coalesce(agents.path, excluded.path)
         RETURNING id",
    )?
    .query_row(params![session_id, agent_id, kind, path], |r| r.get(0))
}

fn agent_row(conn: &Connection, session_id: &str, agent_id: Option<&str>) -> Result<Option<i64>> {
    conn.prepare_cached("SELECT id FROM agents WHERE session_id = ?1 AND agent_id IS ?2")?
        .query_row(params![session_id, agent_id], |r| r.get(0))
        .optional()
}

/// The row of the agent `r` names, if stored.
fn ref_row(conn: &Connection, r: &AgentRef) -> Result<Option<i64>> {
    match r {
        AgentRef::Session { session_id } => agent_row(conn, session_id, None),
        AgentRef::Subagent {
            session_id,
            agent_id,
        } => agent_row(conn, session_id, Some(agent_id)),
    }
}

/// What storing one record changed beyond its own row.
#[derive(Default)]
struct Stored {
    /// The request key of a request record.
    request: Option<String>,
    /// A cross-session message, sent or received.
    message: bool,
    /// A `claude --bg` launch, or a launch's result.
    launch: bool,
}

/// Stores one record; a record not seen before also updates its agent
/// (latest wins) and any GitHub link it carries.
fn record(
    conn: &Connection,
    owner: &Owner,
    agent: i64,
    file: &str,
    offset: u64,
    value: &Value,
) -> Result<Stored> {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let kind = text("type");
    // An empty name is no name, as in claude-config's audit-lib.
    let named = |key: &str| text(key).filter(|name| !name.is_empty());
    let custom_title = match kind {
        Some("custom-title") => named("customTitle"),
        _ => None,
    };
    let acct = account::fields(value);
    let inserted =
        conn.prepare_cached(
            "INSERT OR IGNORE INTO events (session_id, agent_id, type, subtype, uuid, parent_uuid,
                 request_id, timestamp, file, byte_offset, raw, cwd, git_branch, custom_title,
                 request_key, model, usage, output_tokens, cost_usd, tool_uses, tool_results,
                 prompt, notification_task, notification_status, message_to, api_error,
                 sent_msg_id, peer_msg_id, peer_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)",
        )?
        .execute(params![
            owner.session_id,
            owner.agent_id,
            kind,
            text("subtype"),
            text("uuid"),
            text("parentUuid"),
            text("requestId"),
            text("timestamp"),
            file,
            offset as i64,
            value.to_string(),
            text("cwd"),
            text("gitBranch"),
            custom_title,
            acct.request_key,
            acct.model,
            acct.usage,
            acct.output_tokens,
            acct.cost_usd,
            acct.tool_uses,
            acct.tool_results,
            acct.prompt,
            acct.notification_task,
            acct.notification_status,
            acct.message_to,
            acct.api_error,
            acct.sent_msg_id,
            acct.peer_msg_id,
            acct.peer_name,
        ])? > 0;
    if !inserted {
        return Ok(Stored::default());
    }
    // A message written after a PermissionRequest hook: the agent moved on,
    // and its wait ends at the record. Bookkeeping (attachments, queue
    // operations, system and file-history records) can be written while
    // the prompt still shows.
    if let (Some("user" | "assistant"), Some(timestamp)) = (kind, text("timestamp")) {
        let answered: Option<i64> = conn
            .prepare_cached(
                "UPDATE agents SET hook_waiting_for = NULL,
                     hook_state = CASE WHEN agent_id IS NULL THEN NULL ELSE 'running' END
                 WHERE id = ?1 AND hook_state = 'needs_you'
                   AND unixepoch(?2, 'subsec') * 1000 > hook_at_ms
                 RETURNING CAST(round(unixepoch(?2, 'subsec') * 1000) AS INTEGER)",
            )?
            .query_row(params![agent, timestamp], |r| r.get(0))
            .optional()?;
        if let Some(end_ms) = answered {
            live::end_wait(conn, agent, end_ms)?;
        }
    }
    if owner.agent_id.is_none()
        && let Some(root) = text("session_id")
    {
        conn.prepare_cached(
            "UPDATE sessions SET process_root = coalesce(process_root, ?2) WHERE session_id = ?1",
        )?
        .execute(params![owner.session_id, root])?;
    }

    let (model, effort) = if kind == Some("assistant") {
        (
            // `<synthetic>` records are made up locally: never the agent's model.
            value
                .pointer("/message/model")
                .and_then(Value::as_str)
                .filter(|m| *m != SYNTHETIC),
            value.get("effort").and_then(scalar_text),
        )
    } else {
        (None, None)
    };
    let name = match kind {
        Some("agent-name") => named("agentName"),
        _ => custom_title,
    };
    let update = [text("cwd"), text("gitBranch"), text("version"), model, name];
    if update.iter().any(Option::is_some) || effort.is_some() {
        conn.prepare_cached(
            "UPDATE agents SET cwd = coalesce(?2, cwd), git_branch = coalesce(?3, git_branch),
                 version = coalesce(?4, version), model = coalesce(?5, model),
                 name = coalesce(?6, name), effort = coalesce(?7, effort)
             WHERE id = ?1",
        )?
        .execute(params![
            agent, update[0], update[1], update[2], update[3], update[4], effort
        ])?;
    }
    if kind == Some("pr-link")
        && let Some(url) = text("prUrl")
    {
        conn.prepare_cached(
            "INSERT INTO github_links (session_id, pr_number, pr_url, pr_repository, linked_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (session_id, pr_url) DO UPDATE SET
                 pr_number = coalesce(excluded.pr_number, pr_number),
                 pr_repository = coalesce(excluded.pr_repository, pr_repository),
                 linked_at = coalesce(excluded.linked_at, linked_at)",
        )?
        .execute(params![
            owner.session_id,
            value.get("prNumber").and_then(Value::as_i64),
            url,
            text("prRepository"),
            text("timestamp"),
        ])?;
    }
    // A dispatch's tool_result: its subagent's dispatch result, the first
    // stored (the graph's edge looks it up when it comes later).
    if let Some(results) = &acct.tool_results {
        let status = match value["toolUseResult"]["status"].as_str() {
            Some("async_launched") => "async_launched",
            _ => "answered",
        };
        conn.prepare_cached(DISPATCH_ANSWERED_SQL)?
            .execute(params![results, status])?;
    }
    let launch = if acct.tool_uses.is_some() || acct.tool_results.is_some() {
        model::launch_record(conn, agent, value, true)?
    } else {
        false
    };
    Ok(Stored {
        request: acct.usage.and(acct.request_key),
        message: acct.sent_msg_id.is_some() || acct.peer_msg_id.is_some(),
        launch,
    })
}

/// Sets dispatch result `?2` on each subagent not yet answered whose
/// dispatch one of tool_result ids `?1` (a JSON array) answers, the ids
/// outermost (CROSS JOIN) so each finds its edge by index.
const DISPATCH_ANSWERED_SQL: &str = "UPDATE agents SET dispatch_result = ?2
    WHERE dispatch_result IS NULL AND id IN (SELECT d.child FROM json_each(?1) AS j
        CROSS JOIN edges AS d ON d.kind = 'dispatch' AND d.tool_use_id = j.value)";

/// Sets agent row `id`'s meta `finished`: true sticks, anything else
/// changes nothing already stored.
fn set_finished(conn: &Connection, id: i64, finished: Option<bool>) -> Result<()> {
    conn.prepare_cached(
        "UPDATE agents SET finished = CASE WHEN ?2 IS 1 THEN 1 ELSE coalesce(finished, ?2) END
         WHERE id = ?1",
    )?
    .execute(params![id, finished])?;
    Ok(())
}

/// Stores the sessions, agents and edges of a discovery graph. Only adds
/// and fills: nothing already stored is removed, so rows outlive the
/// transcripts they came from. Meta values fill gaps; values read from
/// transcript records stay on top. Then re-resolves the graph's agents in
/// the model and rebuilds the message edges.
pub(super) fn graph(conn: &Connection, g: &Graph) -> Result<()> {
    for s in &g.sessions {
        let path = text_of(&s.path);
        upsert_session(conn, &s.session_id, Some(&s.project_dir), Some(&path))?;
        let id = ensure_agent(conn, &s.session_id, None, "orchestrator", Some(&path))?;
        conn.prepare_cached(
            "UPDATE agents SET cwd = coalesce(cwd, ?2), git_branch = coalesce(git_branch, ?3),
                 version = coalesce(version, ?4)
             WHERE id = ?1",
        )?
        .execute(params![id, s.cwd, s.git_branch, s.version])?;
    }
    for sub in &g.subagents {
        let path = text_of(&sub.path);
        let project_dir = owner_of(&sub.path).project_dir;
        upsert_session(conn, &sub.session_id, project_dir.as_deref(), None)?;
        ensure_agent(conn, &sub.session_id, None, "orchestrator", None)?;
        let kind = match sub.link {
            Link::Teammate { .. } => "teammate",
            _ => "subagent",
        };
        let id = ensure_agent(
            conn,
            &sub.session_id,
            Some(&sub.agent_id),
            kind,
            Some(&path),
        )?;
        let meta = sub.meta.clone().unwrap_or_default();
        // A teammate's meta `agentType` is its name, so it takes the type it
        // was spawned as, else `teammate` (#86). Every scan rewrites it, so a
        // store whose teammates were typed by name is corrected on the next
        // (migration 0019 retypes those whose meta is gone).
        let agent_type = match sub.link {
            Link::Teammate { .. } => Some(
                meta.spawned_agent_type
                    .unwrap_or_else(|| "teammate".to_string()),
            ),
            _ => meta.agent_type,
        };
        conn.prepare_cached(
            "UPDATE agents SET kind = ?2, agent_type = coalesce(?3, agent_type),
                 description = coalesce(?4, description), spawn_depth = coalesce(?5, spawn_depth),
                 team_name = coalesce(?6, team_name), model = coalesce(model, ?7),
                 effort = coalesce(effort, ?8), name = coalesce(name, ?9)
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            kind,
            agent_type,
            meta.description,
            meta.spawn_depth.map(|d| d as i64),
            meta.team_name,
            meta.model,
            meta.effort,
            meta.name,
        ])?;
        set_finished(conn, id, meta.finished)?;
    }
    // Every agent exists now, so parents listed after their children
    // resolve.
    for sub in &g.subagents {
        let child = ensure_agent(conn, &sub.session_id, Some(&sub.agent_id), "subagent", None)?;
        match &sub.link {
            Link::Dispatch {
                parent,
                tool_use_id,
            } => {
                let parent = ref_row(conn, parent)?;
                edge(
                    conn,
                    "dispatch",
                    parent,
                    child,
                    Some(tool_use_id),
                    None,
                    None,
                )?;
            }
            // Only meta `parentAgentId` names the parent: a dispatch with no
            // tool_use_id.
            Link::ParentAgent { parent } => {
                let parent = ref_row(conn, parent)?;
                edge(conn, "dispatch", parent, child, None, None, None)?;
            }
            Link::Teammate {
                team_name,
                team_dir,
            } => {
                let lead = g
                    .teams
                    .iter()
                    .find(|t| Some(&t.dir_name) == team_dir.as_ref())
                    .and_then(|t| t.lead_session_id.as_deref());
                let lead = match lead {
                    Some(session_id) => agent_row(conn, session_id, None)?,
                    None => None,
                };
                edge(
                    conn,
                    "teammate",
                    lead,
                    child,
                    None,
                    Some(team_name),
                    team_dir.as_deref(),
                )?;
                // A lead that doesn't resolve (a team config gone, or naming
                // a session with no transcript, #81), and none stored: the
                // orchestrator of the session whose subagents/ the
                // teammate's transcript is in. A lead resolved later
                // replaces it.
                if lead.is_none() {
                    let fallback = agent_row(conn, &sub.session_id, None)?;
                    conn.prepare_cached(
                        "UPDATE edges SET parent = ?2
                         WHERE kind = 'teammate' AND child = ?1 AND parent IS NULL",
                    )?
                    .execute(params![child, fallback])?;
                }
            }
            Link::Unresolved(why) => {
                let why = match why {
                    Unresolved::NoMeta => "no_meta",
                    Unresolved::NoToolUseId => "no_tool_use_id",
                    Unresolved::ToolUseNotFound { .. } => "tool_use_not_found",
                };
                // An edge stored by an earlier scan outlives the tool_use
                // that made it.
                conn.prepare_cached(
                    "UPDATE agents SET unresolved = ?2
                     WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM edges WHERE child = ?1)",
                )?
                .execute(params![child, why])?;
            }
        }
    }
    let sessions = g.sessions.iter().map(|s| s.session_id.as_str());
    model::graph(
        conn,
        sessions.chain(g.subagents.iter().map(|s| s.session_id.as_str())),
    )
}

fn edge(
    conn: &Connection,
    kind: &str,
    parent: Option<i64>,
    child: i64,
    tool_use_id: Option<&str>,
    team_name: Option<&str>,
    team_dir: Option<&str>,
) -> Result<()> {
    // A dispatch edge stored with this tool_use already has its result:
    // ingest sets it from a tool_result stored after the edge.
    let known = kind == "dispatch"
        && conn
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM edges
                     WHERE kind = 'dispatch' AND child = ?1 AND tool_use_id IS ?2)",
            )?
            .query_row(params![child, tool_use_id], |r| r.get::<_, bool>(0))?;
    conn.prepare_cached(
        "INSERT INTO edges (kind, parent, child, tool_use_id, team_name, team_dir)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (kind, child) DO UPDATE SET
             parent = coalesce(excluded.parent, parent),
             tool_use_id = coalesce(excluded.tool_use_id, tool_use_id),
             team_name = coalesce(excluded.team_name, team_name),
             team_dir = coalesce(excluded.team_dir, team_dir)",
    )?
    .execute(params![
        kind,
        parent,
        child,
        tool_use_id,
        team_name,
        team_dir
    ])?;
    conn.prepare_cached("UPDATE agents SET unresolved = NULL WHERE id = ?1")?
        .execute([child])?;
    // A new dispatch whose tool_result was stored first: its result now.
    if let ("dispatch", Some(tool_use_id), false) = (kind, tool_use_id, known) {
        conn.prepare_cached(
            "UPDATE agents SET dispatch_result = (
                 SELECT CASE WHEN json_extract(e.raw, '$.toolUseResult.status') IS 'async_launched'
                     THEN 'async_launched' ELSE 'answered' END
                 FROM events AS e
                 WHERE e.session_id = agents.session_id AND e.tool_results IS NOT NULL
                     AND instr(e.tool_results, ?2) > 0
                 ORDER BY e.id LIMIT 1)
             WHERE id = ?1 AND dispatch_result IS NULL",
        )?
        .execute(params![child, format!("\"{tool_use_id}\"")])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, query_plan};
    use crate::tail::test_dir;

    /// A tool_result's ids find their dispatch edges by index, not by
    /// walking every dispatch edge per record (#108).
    #[test]
    fn a_tool_result_finds_its_dispatch_by_index() {
        let dir = test_dir("ingest-dispatch-plan");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        assert_eq!(
            query_plan(&conn, DISPATCH_ANSWERED_SQL),
            [
                "SEARCH agents USING INTEGER PRIMARY KEY (rowid=?)",
                "LIST SUBQUERY 1",
                "SCAN j VIRTUAL TABLE INDEX 1:",
                "SEARCH d USING INDEX edges_tool_use (tool_use_id=?)",
            ]
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn owner_comes_from_the_transcript_layout() {
        let session = owner_of(Path::new("/cfg/projects/-work-x/s1.jsonl"));
        assert_eq!(session.session_id, "s1");
        assert_eq!(session.agent_id, None);
        assert_eq!(session.project_dir.as_deref(), Some("-work-x"));

        let sub = owner_of(Path::new(
            "/cfg/projects/-work-x/s1/subagents/agent-a7.jsonl",
        ));
        assert_eq!(sub.session_id, "s1");
        assert_eq!(sub.agent_id.as_deref(), Some("a7"));
        assert_eq!(sub.project_dir.as_deref(), Some("-work-x"));
    }
}
