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

fn registry_row(reader: &Connection, key: &str) -> (i64, i64) {
    reader
        .query_row(
            "SELECT gone, last_seen_ms FROM registry WHERE key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("registry row")
}

#[test]
fn a_baseline_marks_gone_the_live_rows_it_does_not_list() {
    let temp = Temp::new("registry-baseline");
    let (mut store, reader) = temp.open();
    let alpha = with_status(&waiting_alpha(), "busy");
    let stale = entry(
        r#"{"pid":4102,"cwd":"/work/gamma","kind":"interactive","startedAt":1791000001000,
            "sessionId":"s-gamma","name":"gamma-1","status":"idle"}"#,
    );
    // An earlier run stored both; this run's first poll lists only alpha.
    store
        .apply_registry(&[
            RegistryEvent::Appeared {
                entry: alpha.clone(),
                at_ms: 1000,
            },
            RegistryEvent::Appeared {
                entry: stale.clone(),
                at_ms: 1000,
            },
        ])
        .expect("earlier run");
    store
        .apply_registry(&[
            RegistryEvent::Appeared {
                entry: alpha.clone(),
                at_ms: 9000,
            },
            RegistryEvent::Baseline {
                keys: vec![alpha.key()],
            },
        ])
        .expect("first poll");

    assert_eq!(registry_row(&reader, &alpha.key()), (0, 9000));
    assert_eq!(
        registry_row(&reader, &stale.key()),
        (1, 1000),
        "gone keeps the last poll that saw it"
    );
    assert_eq!(
        orchestrator(&reader, "s-gamma"),
        Some((Some("gamma-1".into()), Some("gone".into())))
    );
    assert_eq!(
        orchestrator(&reader, "s-alpha"),
        Some((Some("alpha-1".into()), Some("busy".into())))
    );
}

#[test]
fn a_finished_background_entry_is_stored_but_not_live() {
    let temp = Temp::new("registry-finished");
    let (mut store, reader) = temp.open();
    let done = entry(
        r#"{"id":"bg-2","cwd":"/work/beta","kind":"background","startedAt":1791000000600,
            "sessionId":"s-beta","name":"beta-3","state":"done"}"#,
    );
    store
        .apply_registry(&[
            RegistryEvent::Appeared {
                entry: done.clone(),
                at_ms: 10,
            },
            RegistryEvent::Baseline {
                keys: vec![done.key()],
            },
        ])
        .expect("apply");
    assert_eq!(registry_row(&reader, "id:bg-2"), (1, 10));
    let state: String = reader
        .query_row(
            "SELECT state FROM registry WHERE key = 'id:bg-2'",
            [],
            |r| r.get(0),
        )
        .expect("state");
    assert_eq!(state, "done");
    assert_eq!(
        orchestrator(&reader, "s-beta"),
        Some((Some("beta-3".into()), Some("gone".into())))
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

/// (session, agent id, tool, start ms, end ms) of a stored permission wait.
type Wait = (String, Option<String>, Option<String>, i64, Option<i64>);

/// Every stored permission wait, oldest first.
fn waits(reader: &Connection) -> Vec<Wait> {
    let mut stmt = reader
        .prepare(
            "SELECT a.session_id, a.agent_id, w.tool_name, w.start_ms, w.end_ms
             FROM permission_waits AS w JOIN agents AS a ON a.id = w.agent
             ORDER BY w.start_ms, w.id",
        )
        .expect("query");
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .expect("rows")
    .collect::<rusqlite::Result<_>>()
    .expect("waits")
}

fn wait(agent_id: Option<&str>, tool: &str, start: i64, end: Option<i64>) -> Wait {
    (
        "s-alpha".into(),
        agent_id.map(Into::into),
        Some(tool.into()),
        start,
        end,
    )
}

fn permission(at_ms: i64, agent_id: Option<&str>, tool: &str) -> Hook {
    hook(
        at_ms,
        HookEvent::PermissionRequest {
            session_id: "s-alpha".into(),
            agent_id: agent_id.map(Into::into),
            tool_name: Some(tool.into()),
        },
    )
}

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
fn a_subagent_permission_request_is_needs_you_and_a_stored_wait_until_it_moves_on_or_stops() {
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
    assert_eq!(waits(&reader), [wait(Some("a1"), "Bash", asked, None)]);

    // A record written before the request (the tool_use itself) changes nothing.
    let file = "s-alpha/subagents/agent-a1.jsonl";
    store
        .ingest(&batch(
            file,
            &[json!({"type":"assistant","uuid":"a1-u1","timestamp":"2026-10-08T10:00:04.900Z"})],
        ))
        .expect("earlier record");
    assert_eq!(needs_you(&reader), expected);
    assert_eq!(waits(&reader), [wait(Some("a1"), "Bash", asked, None)]);

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
    let answered = ms("2026-10-08T10:00:09.000Z");
    assert_eq!(
        waits(&reader),
        [wait(Some("a1"), "Bash", asked, Some(answered))]
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
    assert_eq!(
        waits(&reader),
        [
            wait(Some("a1"), "Bash", asked, Some(answered)),
            wait(Some("a1"), "Write", asked + 10_000, Some(asked + 20_000)),
        ]
    );
}

#[test]
fn a_repeated_request_with_no_record_between_is_one_wait_from_the_first() {
    let temp = Temp::new("hook-repeat");
    let (mut store, reader) = temp.open();
    let asked = ms("2026-10-08T10:00:05.000Z");
    store
        .apply_hook(&permission(asked, Some("a1"), "Bash"))
        .expect("first");
    store
        .apply_hook(&permission(asked + 3_000, Some("a1"), "Write"))
        .expect("second");
    assert_eq!(waits(&reader), [wait(Some("a1"), "Bash", asked, None)]);
    store
        .ingest(&batch(
            "s-alpha/subagents/agent-a1.jsonl",
            &[json!({"type":"user","uuid":"a1-u1","timestamp":"2026-10-08T10:00:20.000Z"})],
        ))
        .expect("answered");
    assert_eq!(
        waits(&reader),
        [wait(
            Some("a1"),
            "Bash",
            asked,
            Some(ms("2026-10-08T10:00:20.000Z"))
        )]
    );
}

#[test]
fn any_later_hook_on_the_agent_ends_its_wait_and_other_agents_keep_theirs() {
    let temp = Temp::new("hook-later");
    let (mut store, reader) = temp.open();
    store
        .apply_hook(&permission(100, Some("a1"), "Bash"))
        .expect("a1");
    store
        .apply_hook(&permission(150, Some("a2"), "Edit"))
        .expect("a2");
    store
        .apply_hook(&hook(
            200,
            HookEvent::SubagentStart {
                session_id: "s-alpha".into(),
                agent_id: "a1".into(),
                agent_type: None,
            },
        ))
        .expect("a1 start");
    store
        .apply_hook(&hook(
            300,
            HookEvent::TeammateIdle {
                session_id: "s-alpha".into(),
                agent_id: Some("a2".into()),
                teammate_name: None,
            },
        ))
        .expect("a2 idle");
    assert_eq!(
        waits(&reader),
        [
            wait(Some("a1"), "Bash", 100, Some(200)),
            wait(Some("a2"), "Edit", 150, Some(300)),
        ]
    );
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
    assert_eq!(waits(&reader), [wait(None, "Bash", 50, None)]);
    // The registry still says waiting: the wait goes on.
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: waiting_alpha(),
            at_ms: 55,
        }])
        .expect("waiting");
    assert_eq!(waits(&reader), [wait(None, "Bash", 50, None)]);
    store
        .apply_registry(&[RegistryEvent::Changed {
            entry: with_status(&waiting_alpha(), "busy"),
            at_ms: 60,
        }])
        .expect("busy");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(waits(&reader), [wait(None, "Bash", 50, Some(60))]);

    // A poll stamped before the request arrived (the poller stamps its
    // time before it runs `claude agents`) saw the session before it asked:
    // it answers nothing.
    store
        .apply_hook(&permission(100, None, "Write"))
        .expect("again");
    store
        .apply_registry(&[RegistryEvent::Changed {
            entry: with_status(&waiting_alpha(), "idle"),
            at_ms: 90,
        }])
        .expect("stale idle");
    assert_eq!(needs_you(&reader).len(), 1);
    assert_eq!(
        waits(&reader),
        [
            wait(None, "Bash", 50, Some(60)),
            wait(None, "Write", 100, None),
        ]
    );
    store
        .apply_registry(&[RegistryEvent::Changed {
            entry: with_status(&waiting_alpha(), "busy"),
            at_ms: 110,
        }])
        .expect("busy again");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(
        waits(&reader),
        [
            wait(None, "Bash", 50, Some(60)),
            wait(None, "Write", 100, Some(110)),
        ]
    );
}

#[test]
fn only_a_message_record_ends_a_wait_not_bookkeeping() {
    let temp = Temp::new("hook-bookkeeping");
    let (mut store, reader) = temp.open();
    let asked = ms("2026-10-08T10:00:05.000Z");
    store
        .apply_hook(&permission(asked, None, "Bash"))
        .expect("permission");
    // Written while the prompt shows: none of them is the session acting.
    let later = "2026-10-08T10:00:06.000Z";
    store
        .ingest(&batch(
            "s-alpha.jsonl",
            &[
                json!({"type":"attachment","uuid":"o1","timestamp":later}),
                json!({"type":"queue-operation","operation":"enqueue","timestamp":later}),
                json!({"type":"system","subtype":"informational","uuid":"o2","timestamp":later}),
                json!({"type":"file-history-delta","timestamp":later}),
            ],
        ))
        .expect("bookkeeping");
    assert_eq!(needs_you(&reader).len(), 1);
    assert_eq!(waits(&reader), [wait(None, "Bash", asked, None)]);
    store
        .ingest(&batch(
            "s-alpha.jsonl",
            &[json!({"type":"assistant","uuid":"o3","timestamp":"2026-10-08T10:00:30.000Z"})],
        ))
        .expect("acting");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(
        waits(&reader),
        [wait(
            None,
            "Bash",
            asked,
            Some(ms("2026-10-08T10:00:30.000Z"))
        )]
    );
}

#[test]
fn a_session_gone_mid_prompt_stops_needing_you_and_its_wait_ends_when_last_seen() {
    let temp = Temp::new("hook-gone");
    let (mut store, reader) = temp.open();
    let entry = waiting_alpha();
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: entry.clone(),
            at_ms: 40,
        }])
        .expect("listed");
    store
        .apply_hook(&permission(50, None, "Bash"))
        .expect("orchestrator asks");
    store
        .apply_hook(&permission(52, Some("a1"), "Edit"))
        .expect("subagent asks");
    assert_eq!(needs_you(&reader).len(), 3, "registry, orchestrator, a1");
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry,
            last_seen_ms: 70,
        }])
        .expect("gone");
    assert!(
        needs_you(&reader).is_empty(),
        "a gone session needs nothing"
    );
    assert_eq!(agent(&reader, "s-alpha", None)[0].3, None);
    assert_eq!(
        waits(&reader),
        [
            wait(None, "Bash", 50, Some(70)),
            // A subagent's wait ends on its own record or SubagentStop.
            wait(Some("a1"), "Edit", 52, None),
        ]
    );
}

#[test]
fn a_session_last_seen_before_its_request_ends_the_wait_at_the_request() {
    let temp = Temp::new("hook-gone-early");
    let (mut store, reader) = temp.open();
    let entry = waiting_alpha();
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: entry.clone(),
            at_ms: 40,
        }])
        .expect("listed");
    // Asked between the last poll that listed it and the one that did not.
    store
        .apply_hook(&permission(50, None, "Bash"))
        .expect("asks");
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry,
            last_seen_ms: 40,
        }])
        .expect("gone");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(waits(&reader), [wait(None, "Bash", 50, Some(50))]);
}

#[test]
fn a_baseline_that_drops_a_waiting_session_ends_its_wait_when_last_seen() {
    let temp = Temp::new("hook-baseline");
    let (mut store, reader) = temp.open();
    store
        .apply_registry(&[RegistryEvent::Appeared {
            entry: waiting_alpha(),
            at_ms: 40,
        }])
        .expect("listed");
    store
        .apply_hook(&permission(50, None, "Bash"))
        .expect("asks");
    store
        .apply_registry(&[RegistryEvent::Changed {
            entry: waiting_alpha(),
            at_ms: 60,
        }])
        .expect("still waiting");
    // strate restarts; its first poll no longer lists the session.
    store
        .apply_registry(&[RegistryEvent::Baseline { keys: vec![] }])
        .expect("baseline");
    assert!(needs_you(&reader).is_empty());
    assert_eq!(waits(&reader), [wait(None, "Bash", 50, Some(60))]);
}

#[test]
fn a_gone_row_leaves_a_session_another_live_row_lists_waiting() {
    let temp = Temp::new("hook-gone-other");
    let (mut store, reader) = temp.open();
    let first = waiting_alpha();
    let second = entry(
        r#"{"pid":4102,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
            "sessionId":"s-alpha","name":"alpha-1","status":"waiting","waitingFor":"permission"}"#,
    );
    store
        .apply_registry(&[
            RegistryEvent::Appeared {
                entry: first.clone(),
                at_ms: 40,
            },
            RegistryEvent::Appeared {
                entry: second,
                at_ms: 40,
            },
        ])
        .expect("listed twice");
    store
        .apply_hook(&permission(50, None, "Bash"))
        .expect("asks");
    store
        .apply_registry(&[RegistryEvent::Gone {
            entry: first,
            last_seen_ms: 70,
        }])
        .expect("one gone");
    assert_eq!(waits(&reader), [wait(None, "Bash", 50, None)]);
    assert!(needs_you(&reader).iter().any(|n| n.2 == "hook"));
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
