//! Live state in the store: registry polls and hook events applied over
//! synthetic entries and payloads, alongside tail ingest of the same
//! sessions and agents. Every db lives in a temp dir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use strate_core::hooks::{Hook, HookEvent};
use strate_core::registry::{RegistryEntry, RegistryEvent, parse_snapshot};
use strate_core::store::Store;
use strate_core::tail::{Batch, TailEvent};

/// A temp dir removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-live-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn open(&self) -> (Store, Connection) {
        let db = self.0.join("strate.db");
        let store = Store::open(&db).expect("store");
        let reader = Store::open_reader(&db).expect("reader");
        (store, reader)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn entry(json: &str) -> RegistryEntry {
    parse_snapshot(format!("[{json}]").as_bytes())
        .expect("entry")
        .remove(0)
}

fn waiting_alpha() -> RegistryEntry {
    entry(
        r#"{"pid":4101,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
            "sessionId":"s-alpha","name":"alpha-1","status":"waiting","waitingFor":"permission"}"#,
    )
}

fn with_status(e: &RegistryEntry, status: &str) -> RegistryEntry {
    let mut json = json!({
        "pid": e.pid, "cwd": e.cwd, "kind": e.kind.as_str(), "startedAt": e.started_at,
        "sessionId": e.session_id, "name": e.name, "status": status,
    });
    json.as_object_mut()
        .expect("object")
        .retain(|_, v| !v.is_null());
    entry(&json.to_string())
}

/// One tail batch holding `records` for the transcript at `rel`.
fn batch(rel: &str, records: &[serde_json::Value]) -> Batch {
    let path: Arc<Path> = Path::new("/cfg/projects/-work-alpha").join(rel).into();
    let events = records
        .iter()
        .enumerate()
        .map(|(i, value)| TailEvent::Record {
            path: path.clone(),
            offset: i as u64 * 100,
            value: value.clone(),
        })
        .collect();
    Batch {
        path,
        events,
        checkpoint: None,
    }
}

/// Unix ms of an ISO timestamp, via SQLite like the store reads them.
fn ms(iso: &str) -> i64 {
    Connection::open_in_memory()
        .expect("db")
        .query_row(
            "SELECT CAST(unixepoch(?1, 'subsec') * 1000 AS INTEGER)",
            [iso],
            |r| r.get(0),
        )
        .expect("ms")
}

type NeedsYou = (Option<String>, Option<String>, String, Option<String>, i64);

fn needs_you(reader: &Connection) -> Vec<NeedsYou> {
    let mut stmt = reader
        .prepare(
            "SELECT session_id, agent_id, source, waiting_for, since_ms FROM needs_you
             ORDER BY session_id, agent_id",
        )
        .expect("query");
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .expect("rows")
    .collect::<rusqlite::Result<_>>()
    .expect("needs_you")
}

/// (registry_name, registry_state) of a session's orchestrator.
fn orchestrator(reader: &Connection, session: &str) -> Option<(Option<String>, Option<String>)> {
    reader
        .query_row(
            "SELECT registry_name, registry_state FROM agents
             WHERE session_id = ?1 AND agent_id IS NULL",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .expect("orchestrator")
}

#[test]
fn a_registry_entry_shows_before_its_transcript_and_waiting_is_needs_you() {
    let temp = Temp::new("registry-first");
    let (mut store, reader) = temp.open();
    let alpha = waiting_alpha();
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: alpha.clone(),
            at_ms: 1000,
        }])
        .expect("appeared");

    let row: (
        String,
        String,
        Option<i64>,
        Option<String>,
        String,
        String,
        i64,
        i64,
    ) = reader
        .query_row(
            "SELECT kind, name, pid, bg_id, status, waiting_for, last_seen_ms, gone
             FROM registry WHERE session_id = 's-alpha'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .expect("registry row");
    assert_eq!(
        row,
        (
            "interactive".into(),
            "alpha-1".into(),
            Some(4101),
            None,
            "waiting".into(),
            "permission".into(),
            1000,
            0
        )
    );
    // No transcript yet: a stub session with its orchestrator.
    let stub: (i64, Option<String>) = reader
        .query_row(
            "SELECT stub, path FROM sessions WHERE session_id = 's-alpha'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    assert_eq!(stub, (1, None));
    assert_eq!(
        orchestrator(&reader, "s-alpha"),
        Some((Some("alpha-1".into()), Some("waiting".into())))
    );
    assert_eq!(
        needs_you(&reader),
        [(
            Some("s-alpha".into()),
            None,
            "registry".into(),
            Some("permission".into()),
            1000
        )]
    );

    store
        .apply_registry(&[RegistryEvent::Changed {
            entry: with_status(&alpha, "busy"),
            at_ms: 2000,
        }])
        .expect("changed");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(
        orchestrator(&reader, "s-alpha"),
        Some((Some("alpha-1".into()), Some("busy".into())))
    );

    // The transcript arrives: the same session and orchestrator, filled.
    store
        .ingest(&batch(
            "s-alpha.jsonl",
            &[json!({"type":"user","uuid":"u1","timestamp":"2026-10-08T10:00:00.000Z"})],
        ))
        .expect("ingest");
    let filled: (i64, i64) = reader
        .query_row(
            "SELECT s.stub, (SELECT count(*) FROM agents WHERE session_id = s.session_id)
             FROM sessions AS s WHERE session_id = 's-alpha'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    assert_eq!(filled, (0, 1));
    assert_eq!(
        orchestrator(&reader, "s-alpha"),
        Some((Some("alpha-1".into()), Some("busy".into())))
    );

    store
        .apply_registry(&[RegistryEvent::Gone {
            entry: with_status(&alpha, "busy"),
            last_seen_ms: 3000,
        }])
        .expect("gone");
    let gone: (i64, i64) = reader
        .query_row(
            "SELECT gone, last_seen_ms FROM registry WHERE session_id = 's-alpha'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("gone row");
    assert_eq!(gone, (1, 3000));
    assert_eq!(
        orchestrator(&reader, "s-alpha"),
        Some((Some("alpha-1".into()), Some("gone".into())))
    );
}

#[test]
fn background_entries_keep_their_state_and_unnamed_sessions_still_list() {
    let temp = Temp::new("registry-background");
    let (mut store, reader) = temp.open();
    let background = entry(
        r#"{"id":"bg-1","cwd":"/work/beta","kind":"background","startedAt":1791000000500,
            "sessionId":"s-beta","name":"beta-2","state":"running"}"#,
    );
    let anonymous = entry(
        r#"{"pid":4102,"cwd":"/work/gamma","kind":"interactive","startedAt":1791000001000,
            "status":"waiting","waitingFor":"input"}"#,
    );
    store
        .apply_registry(&[
            RegistryEvent::Appeared {
                entry: background,
                at_ms: 10,
            },
            RegistryEvent::Appeared {
                entry: anonymous,
                at_ms: 10,
            },
        ])
        .expect("apply");
    assert_eq!(
        orchestrator(&reader, "s-beta"),
        Some((Some("beta-2".into()), Some("running".into())))
    );
    let bg: (String, String) = reader
        .query_row(
            "SELECT bg_id, state FROM registry WHERE session_id = 's-beta'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("bg row");
    assert_eq!(bg, ("bg-1".into(), "running".into()));
    let sessions: i64 = reader
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .expect("count");
    assert_eq!(sessions, 1, "an entry with no sessionId makes no session");
    assert_eq!(
        needs_you(&reader),
        [(None, None, "registry".into(), Some("input".into()), 10)]
    );
}

#[test]
fn a_clear_moves_the_registry_entry_to_the_new_session() {
    let temp = Temp::new("registry-clear");
    let (mut store, reader) = temp.open();
    let before = with_status(&waiting_alpha(), "idle");
    let after = entry(
        r#"{"pid":4101,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
            "sessionId":"s-alpha-2","name":"alpha-1","status":"busy"}"#,
    );
    assert_eq!(before.key(), after.key());
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: before,
            at_ms: 1,
        }])
        .expect("appeared");
    store
        .apply_registry(&[RegistryEvent::Changed {
            entry: after,
            at_ms: 2,
        }])
        .expect("changed");
    assert_eq!(orchestrator(&reader, "s-alpha"), Some((None, None)));
    assert_eq!(
        orchestrator(&reader, "s-alpha-2"),
        Some((Some("alpha-1".into()), Some("busy".into())))
    );
}

fn hook(at_ms: i64, event: HookEvent) -> Hook {
    Hook {
        received_ms: at_ms,
        event,
    }
}

type AgentRow = (String, Option<String>, Option<String>, Option<String>);

/// kind, agent_type, path, hook_state of one agent.
fn agent(reader: &Connection, session: &str, agent_id: Option<&str>) -> Vec<AgentRow> {
    let mut stmt = reader
        .prepare(
            "SELECT kind, agent_type, path, hook_state FROM agents
             WHERE session_id = ?1 AND agent_id IS ?2",
        )
        .expect("query");
    stmt.query_map(params![session, agent_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .expect("rows")
    .collect::<rusqlite::Result<_>>()
    .expect("agent")
}

#[test]
fn subagent_start_shows_the_agent_before_its_transcript_and_ingest_merges_into_it() {
    let temp = Temp::new("hook-start");
    let (mut store, reader) = temp.open();
    store
        .apply_hook(&hook(
            1,
            HookEvent::SubagentStart {
                session_id: "s-alpha".into(),
                agent_id: "a1".into(),
                agent_type: Some("implementer".into()),
            },
        ))
        .expect("start");
    assert_eq!(
        agent(&reader, "s-alpha", Some("a1")),
        [(
            "subagent".into(),
            Some("implementer".into()),
            None,
            Some("running".into())
        )]
    );
    assert_eq!(
        agent(&reader, "s-alpha", None).len(),
        1,
        "orchestrator stub"
    );

    store
        .ingest(&batch(
            "s-alpha/subagents/agent-a1.jsonl",
            &[json!({"type":"user","uuid":"a1-u1","timestamp":"2026-10-08T10:00:00.000Z"})],
        ))
        .expect("ingest");
    let rows = agent(&reader, "s-alpha", Some("a1"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1.as_deref(), Some("implementer"));
    assert!(
        rows[0]
            .2
            .as_deref()
            .is_some_and(|p| p.ends_with("agent-a1.jsonl"))
    );
    assert_eq!(rows[0].3.as_deref(), Some("running"));
}

#[test]
fn a_subagent_permission_request_is_needs_you_until_it_moves_on_or_stops() {
    let temp = Temp::new("hook-permission");
    let (mut store, reader) = temp.open();
    let asked = ms("2026-10-08T10:00:05.000Z");
    store
        .apply_hook(&hook(
            asked,
            HookEvent::PermissionRequest {
                session_id: "s-alpha".into(),
                agent_id: Some("a1".into()),
                tool_name: Some("Bash".into()),
            },
        ))
        .expect("permission");
    let expected = [(
        Some("s-alpha".into()),
        Some("a1".into()),
        "hook".into(),
        Some("Bash".into()),
        asked,
    )];
    assert_eq!(needs_you(&reader), expected);

    // A record written before the request (the tool_use itself) changes nothing.
    let file = "s-alpha/subagents/agent-a1.jsonl";
    store
        .ingest(&batch(
            file,
            &[json!({"type":"assistant","uuid":"a1-u1","timestamp":"2026-10-08T10:00:04.900Z"})],
        ))
        .expect("earlier record");
    assert_eq!(needs_you(&reader), expected);

    // A later record (the tool result): the agent moved on.
    store
        .ingest(&batch(
            file,
            &[json!({"type":"user","uuid":"a1-u2","timestamp":"2026-10-08T10:00:09.000Z"})],
        ))
        .expect("later record");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(
        agent(&reader, "s-alpha", Some("a1"))[0].3.as_deref(),
        Some("running")
    );

    store
        .apply_hook(&hook(
            asked + 10_000,
            HookEvent::PermissionRequest {
                session_id: "s-alpha".into(),
                agent_id: Some("a1".into()),
                tool_name: Some("Write".into()),
            },
        ))
        .expect("permission again");
    store
        .apply_hook(&hook(
            asked + 20_000,
            HookEvent::SubagentStop {
                session_id: "s-alpha".into(),
                agent_id: "a1".into(),
                agent_type: Some("implementer".into()),
            },
        ))
        .expect("stop");
    assert!(needs_you(&reader).is_empty());
    let stopped = agent(&reader, "s-alpha", Some("a1"));
    assert_eq!(stopped[0].1.as_deref(), Some("implementer"));
    assert_eq!(stopped[0].3.as_deref(), Some("done"));
}

#[test]
fn an_orchestrator_permission_request_clears_when_the_registry_says_busy() {
    let temp = Temp::new("hook-orchestrator");
    let (mut store, reader) = temp.open();
    store
        .apply_hook(&hook(
            50,
            HookEvent::PermissionRequest {
                session_id: "s-alpha".into(),
                agent_id: None,
                tool_name: Some("Bash".into()),
            },
        ))
        .expect("permission");
    assert_eq!(
        needs_you(&reader),
        [(
            Some("s-alpha".into()),
            None,
            "hook".into(),
            Some("Bash".into()),
            50
        )]
    );
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: with_status(&waiting_alpha(), "busy"),
            at_ms: 60,
        }])
        .expect("busy");
    assert!(needs_you(&reader).is_empty());
}

#[test]
fn teammate_idle_marks_the_teammate_or_the_session_idle() {
    let temp = Temp::new("hook-idle");
    let (mut store, reader) = temp.open();
    store
        .apply_hook(&hook(
            1,
            HookEvent::TeammateIdle {
                session_id: "s-team".into(),
                agent_id: Some("t1".into()),
                teammate_name: Some("ipsum".into()),
            },
        ))
        .expect("idle teammate");
    store
        .apply_hook(&hook(
            2,
            HookEvent::TeammateIdle {
                session_id: "s-mate".into(),
                agent_id: None,
                teammate_name: Some("dolor".into()),
            },
        ))
        .expect("idle session");
    let teammate = agent(&reader, "s-team", Some("t1"));
    assert_eq!(teammate[0].0, "teammate");
    assert_eq!(teammate[0].3.as_deref(), Some("idle"));
    assert_eq!(agent(&reader, "s-mate", None)[0].3.as_deref(), Some("idle"));
}
