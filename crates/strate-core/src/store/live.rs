//! Writes live state: registry poll events and hook events. Every function
//! runs inside the caller's transaction. Rows are created the way ingest
//! creates them (a stub session, its orchestrator, the agent), so a later
//! discovery or tail of the same session or agent fills the same rows. A
//! registry row is live while `gone = 0`: listed by the latest poll and not
//! a finished background session. A row describes its process's latest
//! `/clear` link (the `latest_links` view): `claude agents` keys a process
//! by the session it started with, so the row names the first link while
//! a later one runs (#68). The views read its name and state from the row.
//!
//! An agent has an open permission wait exactly while its `hook_state` is
//! `needs_you`: whatever clears that state ends the wait, at the time of
//! the record, poll or hook that cleared it. A process the registry stops
//! listing exited, so its orchestrator's prompt goes with it (#64).

use std::collections::HashSet;

use rusqlite::{Connection, params};

use super::Result;
use super::ingest::{ensure_agent, upsert_session};
use crate::hooks::{Hook, HookEvent};
use crate::registry::{RegistryEntry, RegistryEvent, Status};

pub(super) fn registry(conn: &Connection, event: &RegistryEvent) -> Result<()> {
    match event {
        RegistryEvent::Appeared { entry, at_ms } | RegistryEvent::Changed { entry, at_ms } => {
            upsert_entry(conn, entry, *at_ms)
        }
        RegistryEvent::Gone {
            entry,
            last_seen_ms,
        } => {
            conn.prepare_cached("UPDATE registry SET gone = 1, last_seen_ms = ?2 WHERE key = ?1")?
                .execute(params![entry.key(), last_seen_ms])?;
            exited(conn, entry.session_id.as_deref(), *last_seen_ms)
        }
        RegistryEvent::Baseline { keys } => reconcile(conn, keys),
        RegistryEvent::Error { .. } => Ok(()),
    }
}

/// Marks gone every live row the baseline does not list: entries an
/// earlier run stored that ended while nothing polled. Each keeps the last
/// poll that saw it.
fn reconcile(conn: &Connection, keys: &[String]) -> Result<()> {
    let listed: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let live: Vec<(String, Option<String>, i64)> = conn
        .prepare_cached("SELECT key, session_id, last_seen_ms FROM registry WHERE gone = 0")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (key, session_id, last_seen_ms) in &live {
        if listed.contains(key.as_str()) {
            continue;
        }
        conn.prepare_cached("UPDATE registry SET gone = 1 WHERE key = ?1")?
            .execute([key])?;
        exited(conn, session_id.as_deref(), *last_seen_ms)?;
    }
    Ok(())
}

fn upsert_entry(conn: &Connection, entry: &RegistryEntry, at_ms: i64) -> Result<()> {
    if let Some(session_id) = &entry.session_id {
        upsert_session(conn, session_id, None, None)?;
        ensure_agent(conn, session_id, None, "orchestrator", None)?;
    }
    // A wait on the user keeps the start of the poll that first saw it
    // (#84): a change mid-wait, or a restart listing it again, is the same
    // wait.
    let waits = entry.status == Some(Status::Waiting) || entry.state.as_deref() == Some("blocked");
    conn.prepare_cached(
        "INSERT INTO registry (key, kind, session_id, pid, bg_id, name, cwd, started_at_ms,
             status, waiting_for, state, first_seen_ms, last_seen_ms, gone, waiting_since_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13, ?14)
         ON CONFLICT (key) DO UPDATE SET kind = excluded.kind,
             session_id = excluded.session_id, pid = excluded.pid, bg_id = excluded.bg_id,
             name = excluded.name, cwd = excluded.cwd, started_at_ms = excluded.started_at_ms,
             status = excluded.status, waiting_for = excluded.waiting_for,
             state = excluded.state, last_seen_ms = excluded.last_seen_ms, gone = excluded.gone,
             waiting_since_ms = CASE WHEN excluded.waiting_since_ms IS NOT NULL
                 THEN coalesce(registry.waiting_since_ms, excluded.waiting_since_ms) END",
    )?
    .execute(params![
        entry.key(),
        entry.kind.as_str(),
        entry.session_id,
        entry.pid,
        entry.id,
        entry.name,
        entry.cwd,
        entry.started_at,
        entry.status.as_ref().map(Status::as_str),
        entry.waiting_for,
        entry.state,
        at_ms,
        entry.finished(),
        waits.then_some(at_ms),
    ])?;
    // The registry is the orchestrator's truth: once it is not waiting, a
    // PermissionRequest hook on its latest link's orchestrator is answered.
    // A poll is stamped before it runs, so one stamped before the request
    // saw the session before it asked, and answers nothing.
    if entry.status != Some(Status::Waiting) {
        let answered: Vec<i64> = conn
            .prepare_cached(
                "UPDATE agents SET hook_state = NULL, hook_waiting_for = NULL
                 WHERE session_id = (SELECT latest FROM latest_links WHERE session_id = ?1)
                   AND agent_id IS NULL AND hook_state = 'needs_you' AND hook_at_ms <= ?2
                 RETURNING id",
            )?
            .query_map(params![entry.session_id, at_ms], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for agent in answered {
            end_wait(conn, agent, at_ms)?;
        }
    }
    Ok(())
}

/// Once no live registry row describes the latest link of `session_id`'s
/// chain, its process exited: that link's orchestrator's prompt is gone,
/// and the wait ends when the process was last seen, or at the latest
/// request when that came after (asked between the last poll that listed
/// it and the one that did not). Subagent waits end on their own records
/// or SubagentStop.
fn exited(conn: &Connection, session_id: Option<&str>, last_seen_ms: i64) -> Result<()> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let cleared: Vec<(i64, Option<i64>)> = conn
        .prepare_cached(
            "UPDATE agents SET hook_state = NULL, hook_waiting_for = NULL
             WHERE session_id = (SELECT latest FROM latest_links WHERE session_id = ?1)
               AND agent_id IS NULL AND hook_state = 'needs_you'
               AND NOT EXISTS (SELECT 1 FROM registry AS r
                   JOIN latest_links AS l ON l.session_id = r.session_id
                   WHERE r.gone = 0 AND l.latest = agents.session_id)
             RETURNING id, hook_at_ms",
        )?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (agent, asked) in cleared {
        end_wait(
            conn,
            agent,
            asked.map_or(last_seen_ms, |a| a.max(last_seen_ms)),
        )?;
    }
    Ok(())
}

pub(super) fn hook(conn: &Connection, hook: &Hook) -> Result<()> {
    let (session_id, agent_id, kind, state, waiting_for, agent_type) = match &hook.event {
        HookEvent::SubagentStart {
            session_id,
            agent_id,
            agent_type,
        } => (
            session_id,
            Some(agent_id),
            "subagent",
            "running",
            None,
            agent_type,
        ),
        HookEvent::SubagentStop {
            session_id,
            agent_id,
            agent_type,
        } => (
            session_id,
            Some(agent_id),
            "subagent",
            "done",
            None,
            agent_type,
        ),
        HookEvent::PermissionRequest {
            session_id,
            agent_id,
            tool_name,
        } => (
            session_id,
            agent_id.as_ref(),
            "subagent",
            "needs_you",
            tool_name.as_deref(),
            &None,
        ),
        HookEvent::TeammateIdle {
            session_id,
            agent_id,
            ..
        } => (
            session_id,
            agent_id.as_ref(),
            "teammate",
            "idle",
            None,
            &None,
        ),
    };
    upsert_session(conn, session_id, None, None)?;
    let orchestrator = ensure_agent(conn, session_id, None, "orchestrator", None)?;
    let id = match agent_id {
        Some(agent_id) => ensure_agent(conn, session_id, Some(agent_id), kind, None)?,
        None => orchestrator,
    };
    if state == "needs_you" {
        start_wait(conn, id, waiting_for, hook.received_ms)?;
    } else {
        end_wait(conn, id, hook.received_ms)?;
    }
    conn.prepare_cached(
        "UPDATE agents SET agent_type = coalesce(agent_type, ?2), hook_state = ?3,
             hook_waiting_for = ?4, hook_at_ms = ?5
         WHERE id = ?1",
    )?
    .execute(params![
        id,
        agent_type,
        state,
        waiting_for,
        hook.received_ms
    ])?;
    Ok(())
}

/// Opens a permission wait for `agent` at `start_ms`, unless one is open:
/// a repeat request before the agent moved on is the same wait.
fn start_wait(conn: &Connection, agent: i64, tool: Option<&str>, start_ms: i64) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO permission_waits (agent, tool_name, start_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT DO NOTHING",
    )?
    .execute(params![agent, tool, start_ms])?;
    Ok(())
}

/// Ends `agent`'s open permission wait, if any, at `end_ms`: never before
/// it started, as every caller ends it no earlier than the latest request.
pub(super) fn end_wait(conn: &Connection, agent: i64, end_ms: i64) -> Result<()> {
    conn.prepare_cached(
        "UPDATE permission_waits SET end_ms = ?2 WHERE agent = ?1 AND end_ms IS NULL",
    )?
    .execute(params![agent, end_ms])?;
    Ok(())
}
