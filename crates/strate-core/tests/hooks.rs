//! The optional http hook receiver over real loopback sockets on an
//! OS-chosen port (bind 0). Payloads are synthetic.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use strate_core::hooks::{HookEvent, HookOptions, HookReceiver};

const PATIENCE: Duration = Duration::from_secs(10);

/// Sends one raw request and returns the response status code and body.
fn send(addr: SocketAddr, request: &[u8]) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(PATIENCE)).expect("timeout");
    // The server may answer and close before reading an oversized body.
    let _ = stream.write_all(request);
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no status in {text:?}"));
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

fn post(addr: SocketAddr, body: &str, auth: Option<&str>) -> (u16, String) {
    let auth = auth
        .map(|a| format!("Authorization: {a}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /hooks HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    send(addr, request.as_bytes())
}

fn start(
    options: HookOptions,
) -> (
    HookReceiver,
    strate_core::hooks::Receiver<strate_core::hooks::Hook>,
) {
    HookReceiver::start(HookOptions { port: 0, ..options }).expect("start")
}

const START: &str = r#"{"session_id":"s1","transcript_path":"/work/x/s1.jsonl","cwd":"/work/x",
    "hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"implementer"}"#;

#[test]
fn it_binds_loopback_only_and_answers_204_empty() {
    let (receiver, hooks) = start(HookOptions::default());
    let addr = receiver.local_addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_ne!(addr.port(), 0);

    assert_eq!(post(addr, START, None), (204, String::new()));
    let hook = hooks.recv_timeout(PATIENCE).expect("event");
    assert!(hook.received_ms > 0);
    assert_eq!(
        hook.event,
        HookEvent::SubagentStart {
            session_id: "s1".into(),
            agent_id: "a1".into(),
            agent_type: Some("implementer".into()),
        }
    );
    receiver.stop();
}

#[test]
fn the_four_events_parse_into_typed_events() {
    let (receiver, hooks) = start(HookOptions::default());
    let addr = receiver.local_addr();
    let bodies = [
        r#"{"session_id":"s1","hook_event_name":"SubagentStop","agent_id":"a1",
            "agent_type":"implementer","agent_transcript_path":"/work/x/a1.jsonl",
            "last_assistant_message":"Lorem.","stop_hook_active":false}"#,
        r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"Bash",
            "tool_input":{"command":"echo lorem"}}"#,
        r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"Write",
            "tool_input":{},"agent_id":"a2"}"#,
        r#"{"session_id":"s2","hook_event_name":"TeammateIdle","teammate_name":"ipsum"}"#,
    ];
    for body in bodies {
        assert_eq!(post(addr, body, None).0, 204, "{body}");
    }
    let got: Vec<_> = (0..4)
        .map(|_| hooks.recv_timeout(PATIENCE).expect("event").event)
        .collect();
    assert_eq!(
        got,
        [
            HookEvent::SubagentStop {
                session_id: "s1".into(),
                agent_id: "a1".into(),
                agent_type: Some("implementer".into()),
            },
            HookEvent::PermissionRequest {
                session_id: "s1".into(),
                agent_id: None,
                tool_name: Some("Bash".into()),
            },
            HookEvent::PermissionRequest {
                session_id: "s1".into(),
                agent_id: Some("a2".into()),
                tool_name: Some("Write".into()),
            },
            HookEvent::TeammateIdle {
                session_id: "s2".into(),
                agent_id: None,
                teammate_name: Some("ipsum".into()),
            },
        ]
    );
    receiver.stop();
}

#[test]
fn other_events_are_ignored_with_204() {
    let (receiver, hooks) = start(HookOptions::default());
    let addr = receiver.local_addr();
    let other = r#"{"session_id":"s1","hook_event_name":"PreToolUse","tool_name":"Bash"}"#;
    assert_eq!(post(addr, other, None), (204, String::new()));
    // A later known event is the next thing delivered.
    assert_eq!(post(addr, START, None).0, 204);
    let next = hooks.recv_timeout(PATIENCE).expect("event");
    assert!(matches!(next.event, HookEvent::SubagentStart { .. }));
    receiver.stop();
}

#[test]
fn malformed_requests_get_400_and_the_receiver_keeps_serving() {
    let (receiver, hooks) = start(HookOptions::default());
    let addr = receiver.local_addr();
    for body in [
        "not json",
        "[]",
        r#"{"session_id":"s1"}"#,
        r#"{"session_id":"s1","hook_event_name":"SubagentStart"}"#,
        r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#,
        r#"{"session_id":7,"hook_event_name":"TeammateIdle"}"#,
    ] {
        assert_eq!(post(addr, body, None).0, 400, "{body}");
    }
    assert_eq!(send(addr, b"garbage\r\n\r\n").0, 400);
    assert_eq!(
        send(addr, b"POST / HTTP/1.1\r\nContent-Length: nope\r\n\r\n").0,
        400
    );
    // No Content-Length: the body length is unknown.
    assert_eq!(send(addr, b"POST / HTTP/1.1\r\nHost: x\r\n\r\n{}").0, 411);
    assert!(hooks.try_recv().is_err());
    assert_eq!(post(addr, START, None).0, 204);
    receiver.stop();
}

#[test]
fn only_post_is_accepted() {
    let (receiver, _hooks) = start(HookOptions::default());
    let addr = receiver.local_addr();
    assert_eq!(send(addr, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").0, 405);
    receiver.stop();
}

#[test]
fn a_body_over_the_cap_gets_413() {
    let (receiver, hooks) = start(HookOptions {
        max_body: 64,
        ..HookOptions::default()
    });
    let addr = receiver.local_addr();
    assert!(START.len() > 64);
    assert_eq!(post(addr, START, None).0, 413);
    assert!(hooks.try_recv().is_err());
    receiver.stop();
}

#[test]
fn a_configured_token_is_required_as_a_bearer() {
    let (receiver, hooks) = start(HookOptions {
        token: Some("lorem-token".into()),
        ..HookOptions::default()
    });
    let addr = receiver.local_addr();
    assert_eq!(post(addr, START, None).0, 401);
    assert_eq!(post(addr, START, Some("Bearer wrong-token")).0, 401);
    assert_eq!(post(addr, START, Some("Bearer lorem-tok")).0, 401);
    assert_eq!(post(addr, START, Some("Basic lorem-token")).0, 401);
    assert!(hooks.try_recv().is_err());
    assert_eq!(post(addr, START, Some("Bearer lorem-token")).0, 204);
    assert!(hooks.recv_timeout(PATIENCE).is_ok());
    receiver.stop();
}

#[test]
fn a_full_queue_drops_events_and_never_delays_the_answer() {
    let (receiver, hooks) = start(HookOptions {
        channel_events: 1,
        ..HookOptions::default()
    });
    let addr = receiver.local_addr();
    for _ in 0..3 {
        let sent = Instant::now();
        assert_eq!(post(addr, START, None).0, 204);
        assert!(sent.elapsed() < Duration::from_secs(2));
    }
    assert_eq!(receiver.dropped(), 2);
    assert!(hooks.recv_timeout(PATIENCE).is_ok());
    assert!(hooks.try_recv().is_err());
    receiver.stop();
}

#[test]
fn stop_returns_promptly_and_frees_the_port() {
    let (receiver, _hooks) = start(HookOptions::default());
    let addr = receiver.local_addr();
    let stopping = Instant::now();
    receiver.stop();
    assert!(stopping.elapsed() < Duration::from_secs(2));
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err());
}
