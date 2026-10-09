//! Writes live state: registry poll events and hook events. Every function
//! runs inside the caller's transaction. Rows are created the way ingest
//! creates them (a stub session, its orchestrator, the agent), so a later
//! discovery or tail of the same session or agent fills the same rows.

use rusqlite::{Connection, OptionalExtension, params};

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
            refresh(conn, entry.session_id.as_deref())
        }
        RegistryEvent::Error { .. } => Ok(()),
    }
}

fn upsert_entry(conn: &Connection, entry: &RegistryEntry, at_ms: i64) -> Result<()> {
    let key = entry.key();
    // A /clear keeps the process but moves it to a new session.
    let before: Option<Option<String>> = conn
        .prepare_cached("SELECT session_id FROM registry WHERE key = ?1")?
        .query_row([&key], |r| r.get(0))
        .optional()?;
    if let Some(session_id) = &entry.session_id {
        upsert_session(conn, session_id, None, None)?;
        ensure_agent(conn, session_id, None, "orchestrator", None)?;
    }
    conn.prepare_cached(
        "INSERT INTO registry (key, kind, session_id, pid, bg_id, name, cwd, started_at_ms,
             status, waiting_for, state, first_seen_ms, last_seen_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
         ON CONFLICT (key) DO UPDATE SET kind = excluded.kind,
             session_id = excluded.session_id, pid = excluded.pid, bg_id = excluded.bg_id,
             name = excluded.name, cwd = excluded.cwd, started_at_ms = excluded.started_at_ms,
             status = excluded.status, waiting_for = excluded.waiting_for,
             state = excluded.state, last_seen_ms = excluded.last_seen_ms, gone = 0",
    )?
    .execute(params![
        key,
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
    ])?;
    // The registry is the orchestrator's truth: once it is not waiting, a
    // PermissionRequest hook on the orchestrator is answered.
    if entry.status != Some(Status::Waiting) {
        conn.prepare_cached(
            "UPDATE agents SET hook_state = NULL, hook_waiting_for = NULL
             WHERE session_id = ?1 AND agent_id IS NULL AND hook_state = 'needs_you'",
        )?
        .execute([&entry.session_id])?;
    }
    if let Some(Some(old)) = before
        && entry.session_id.as_ref() != Some(&old)
    {
        refresh(conn, Some(&old))?;
    }
    refresh(conn, entry.session_id.as_deref())
}

/// Recomputes the orchestrator's `registry_name` and `registry_state` from
/// the session's newest registry row: a live one first, `gone` when only
/// gone ones are left, NULL when none lists the session.
fn refresh(conn: &Connection, session_id: Option<&str>) -> Result<()> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    conn.prepare_cached(
        "UPDATE agents SET (registry_name, registry_state) = (
             SELECT name, CASE WHEN gone = 1 THEN 'gone' ELSE coalesce(status, state) END
             FROM registry WHERE session_id = ?1
             ORDER BY gone, last_seen_ms DESC, id DESC LIMIT 1)
         WHERE session_id = ?1 AND agent_id IS NULL",
    )?
    .execute([session_id])?;
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
