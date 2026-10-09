//! The registry poller over scripted `claude agents --json --all` output:
//! no real CLI is ever spawned. Entries are synthetic (placeholder ids,
//! `/work/...` cwds).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use strate_core::registry::{
    Kind, PollOptions, RegistryEvent, RegistryPoller, Runner, Status, parse_snapshot,
    resolve_claude,
};

const PATIENCE: Duration = Duration::from_secs(10);

const INTERACTIVE: &str = r#"{"pid":4101,"cwd":"/work/alpha","kind":"interactive",
    "startedAt":1791000000000,"sessionId":"s-alpha","name":"alpha-1","status":"busy"}"#;
const BACKGROUND: &str = r#"{"id":"bg-1","cwd":"/work/beta","kind":"background",
    "startedAt":1791000000500,"sessionId":"s-beta","name":"beta-2","state":"running"}"#;

fn snapshot(entries: &[&str]) -> Vec<u8> {
    format!("[{}]", entries.join(",")).into_bytes()
}

#[test]
fn both_key_sets_parse_and_unknown_fields_and_values_are_kept_or_ignored() {
    let waiting = r#"{"pid":4102,"cwd":"/work/gamma","kind":"interactive","startedAt":1791000001000,
        "status":"waiting","waitingFor":"permission","futureField":{"x":1}}"#;
    let novel = r#"{"cwd":"/work/delta","kind":"cloud","startedAt":1791000002000,
        "status":"thinking"}"#;
    let entries =
        parse_snapshot(&snapshot(&[INTERACTIVE, BACKGROUND, waiting, novel])).expect("parse");
    assert_eq!(entries.len(), 4);

    let alpha = &entries[0];
    assert_eq!(alpha.kind, Kind::Interactive);
    assert_eq!(alpha.pid, Some(4101));
    assert_eq!(alpha.session_id.as_deref(), Some("s-alpha"));
    assert_eq!(alpha.name.as_deref(), Some("alpha-1"));
    assert_eq!(alpha.status, Some(Status::Busy));
    assert_eq!(alpha.started_at, 1_791_000_000_000);

    let beta = &entries[1];
    assert_eq!(beta.kind, Kind::Background);
    assert_eq!(beta.id.as_deref(), Some("bg-1"));
    assert_eq!(beta.state.as_deref(), Some("running"));
    assert_eq!(beta.status, None);

    let gamma = &entries[2];
    assert_eq!(gamma.status, Some(Status::Waiting));
    assert_eq!(gamma.waiting_for.as_deref(), Some("permission"));
    assert_eq!(gamma.session_id, None);

    let delta = &entries[3];
    assert_eq!(delta.kind, Kind::Other("cloud".into()));
    assert_eq!(delta.status, Some(Status::Other("thinking".into())));
    assert_eq!(delta.kind.as_str(), "cloud");
    assert_eq!(Status::Waiting.as_str(), "waiting");
}

#[test]
fn malformed_output_is_an_error_not_a_panic() {
    for bad in [
        &b"not json"[..],
        b"{\"cwd\":\"/work/x\"}",
        b"[{\"kind\":\"interactive\",\"startedAt\":1}]",
        b"[{\"cwd\":\"/work/x\",\"kind\":\"interactive\",\"startedAt\":\"soon\"}]",
        b"",
    ] {
        assert!(
            parse_snapshot(bad).is_err(),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn each_entry_has_a_stable_identity() {
    let entries = parse_snapshot(&snapshot(&[INTERACTIVE, BACKGROUND])).expect("parse");
    assert_eq!(entries[0].key(), "pid:4101:1791000000000");
    assert_eq!(entries[1].key(), "id:bg-1");
    let bare = parse_snapshot(
        br#"[{"cwd":"/work/x","kind":"interactive","startedAt":7,"sessionId":"s-x"},
             {"cwd":"/work/y","kind":"interactive","startedAt":8}]"#,
    )
    .expect("parse");
    assert_eq!(bare[0].key(), "session:s-x");
    assert_eq!(bare[1].key(), "start:8:/work/y");
}

/// A runner replaying `outputs` in order, then repeating the last one.
fn scripted(outputs: Vec<io::Result<Vec<u8>>>) -> (Runner, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0));
    let counter = calls.clone();
    let mut queue: VecDeque<_> = outputs.into();
    let runner: Runner = Box::new(move || {
        *counter.lock().expect("calls") += 1;
        match queue.len() {
            0 => Ok(b"[]".to_vec()),
            1 => match queue.front().expect("one left") {
                Ok(bytes) => Ok(bytes.clone()),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            },
            _ => queue.pop_front().expect("next"),
        }
    });
    (runner, calls)
}

fn fast() -> PollOptions {
    PollOptions {
        every: Duration::from_millis(10),
        ..PollOptions::default()
    }
}

fn label(event: &RegistryEvent) -> String {
    match event {
        RegistryEvent::Appeared { entry, .. } => format!("appeared {}", entry.key()),
        RegistryEvent::Changed { entry, .. } => format!("changed {}", entry.key()),
        RegistryEvent::Gone { entry, .. } => format!("gone {}", entry.key()),
        RegistryEvent::Error { .. } => "error".into(),
    }
}

#[test]
fn the_poller_emits_appeared_changed_gone_and_error_by_diffing_snapshots() {
    let busy_to_waiting = INTERACTIVE.replace(
        r#""status":"busy""#,
        r#""status":"waiting","waitingFor":"permission""#,
    );
    let (runner, _calls) = scripted(vec![
        Ok(snapshot(&[INTERACTIVE, BACKGROUND])),
        // Unchanged: no events.
        Ok(snapshot(&[INTERACTIVE, BACKGROUND])),
        Ok(snapshot(&[&busy_to_waiting, BACKGROUND])),
        // A malformed poll and a failed run keep the last snapshot.
        Ok(b"garbage".to_vec()),
        Err(io::Error::other("spawn failed")),
        Ok(snapshot(&[BACKGROUND])),
    ]);
    let (poller, events) = RegistryPoller::start(runner, fast()).expect("start");
    let started = Instant::now();
    let mut got = Vec::new();
    while got.len() < 6 {
        let left = PATIENCE.saturating_sub(started.elapsed());
        got.push(events.recv_timeout(left).expect("event in time"));
    }
    // Many more polls of the same snapshot: nothing more is emitted.
    std::thread::sleep(Duration::from_millis(100));
    poller.stop();
    assert!(
        events.try_recv().is_err(),
        "an unchanged registry emits nothing"
    );

    let mut labels: Vec<_> = got.iter().map(label).collect();
    // Order within one poll is not part of the contract.
    labels[..2].sort();
    assert_eq!(
        labels,
        [
            "appeared id:bg-1",
            "appeared pid:4101:1791000000000",
            "changed pid:4101:1791000000000",
            "error",
            "error",
            "gone pid:4101:1791000000000",
        ]
    );
    match &got[2] {
        RegistryEvent::Changed { entry, at_ms } => {
            assert_eq!(entry.status, Some(Status::Waiting));
            assert_eq!(entry.waiting_for.as_deref(), Some("permission"));
            assert!(*at_ms > 0);
        }
        other => panic!("{other:?}"),
    }
    match &got[3] {
        RegistryEvent::Error { message, .. } => assert!(!message.is_empty()),
        other => panic!("{other:?}"),
    }
    match (&got[2], &got[5]) {
        (RegistryEvent::Changed { at_ms, .. }, RegistryEvent::Gone { last_seen_ms, .. }) => {
            assert!(
                last_seen_ms >= at_ms,
                "gone carries the last poll that saw it"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn stop_returns_even_while_the_consumer_is_not_reading() {
    let (runner, calls) = scripted(vec![]);
    // Every poll alternates between two snapshots, so every poll emits.
    let mut flip = false;
    let mut inner = runner;
    let flipping: Runner = Box::new(move || {
        flip = !flip;
        inner()?;
        Ok(if flip {
            snapshot(&[INTERACTIVE])
        } else {
            snapshot(&[])
        })
    });
    let options = PollOptions {
        channel_events: 1,
        ..fast()
    };
    let (poller, events) = RegistryPoller::start(flipping, options).expect("start");
    let started = Instant::now();
    // The first poll fills the channel; the second blocks sending.
    while *calls.lock().expect("calls") < 2 && started.elapsed() < PATIENCE {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopping = Instant::now();
    poller.stop();
    assert!(stopping.elapsed() < Duration::from_secs(2));
    assert!(
        events.len() <= 1,
        "the channel stays bounded: {}",
        events.len()
    );
}

#[test]
fn dropping_the_receiver_ends_the_worker() {
    let (runner, _calls) = scripted(vec![Ok(snapshot(&[INTERACTIVE]))]);
    let (poller, events) = RegistryPoller::start(runner, fast()).expect("start");
    drop(events);
    let stopping = Instant::now();
    poller.stop();
    assert!(stopping.elapsed() < Duration::from_secs(2));
}

struct Bins {
    base: PathBuf,
}

impl Bins {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("strate-reg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("base");
        Self { base }
    }

    fn file(&self, rel: &str) -> PathBuf {
        let path = self.base.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, b"").expect("file");
        path
    }
}

impl Drop for Bins {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

const EXE: &str = if cfg!(windows) {
    "claude.exe"
} else {
    "claude"
};

#[test]
fn the_binary_is_the_explicit_path_else_path_else_local_bin() {
    let bins = Bins::new("resolve");
    let on_path = bins.file(&format!("path-b/{EXE}"));
    let local = bins.file(&format!("home/.local/bin/{EXE}"));
    let home = Some(bins.base.join("home"));
    let path_var: OsString =
        std::env::join_paths([bins.base.join("path-a"), bins.base.join("path-b")]).expect("PATH");

    let explicit = PathBuf::from("/opt/claude/custom");
    assert_eq!(
        resolve_claude(Some(explicit.clone()), Some(path_var.clone()), home.clone()),
        Some(explicit)
    );
    assert_eq!(
        resolve_claude(None, Some(path_var), home.clone()),
        Some(on_path)
    );
    let empty_path = std::env::join_paths([bins.base.join("path-a")]).expect("PATH");
    assert_eq!(
        resolve_claude(None, Some(empty_path), home.clone()),
        Some(local.clone())
    );
    assert_eq!(resolve_claude(None, None, home), Some(local));
    assert_eq!(
        resolve_claude(None, None, Some(bins.base.join("nobody"))),
        None
    );
    assert_eq!(resolve_claude(None, None, None), None);
}
