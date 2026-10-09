//! The ingest engine over synthetic transcripts written to temp dirs at test
//! time, with a fake registry command and the hook receiver on a free port.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use strate_core::hooks::HookOptions;
use strate_core::registry::{PollOptions, Runner};
use strate_core::store::Store;
use strate_core::store::views::{self, AgentState};
use strate_core::tail::TailOptions;
use strate_lib::engine::{Config, Engine, Status};

const PATIENCE: Duration = Duration::from_secs(10);

/// A config-dir root plus a state dir, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-engine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("claude/projects")).expect("root");
        fs::create_dir_all(dir.join("state")).expect("state");
        Self(dir)
    }

    fn root(&self) -> PathBuf {
        self.0.join("claude")
    }

    fn db(&self) -> PathBuf {
        self.0.join("state/strate.db")
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.root().join("projects/-work-alpha").join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, text).expect("write");
        path
    }

    fn config(&self, registry: Option<Runner>, hooks: Option<HookOptions>) -> Config {
        Config {
            root: self.root(),
            db: self.db(),
            registry,
            poll: PollOptions {
                every: Duration::from_millis(50),
                ..PollOptions::default()
            },
            hooks,
            tail: TailOptions {
                rescan_every: Duration::from_millis(100),
                ..TailOptions::default()
            },
            debounce: Duration::from_millis(100),
            rediscover_after: Duration::from_millis(100),
        }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

fn session_lines() -> String {
    [
        line(serde_json::json!({"type": "custom-title", "customTitle": "alpha-1"})),
        line(serde_json::json!({"type": "user", "uuid": "u1",
            "timestamp": "2026-10-08T10:00:00.000Z", "cwd": "/work/alpha",
            "message": {"role": "user", "content": "Lorem ipsum."}})),
        line(serde_json::json!({"type": "assistant", "uuid": "u2",
            "timestamp": "2026-10-08T10:00:10.000Z", "requestId": "req-1",
            "message": {"id": "m1", "model": "claude-haiku-4-5",
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "Agent", "input": {}}],
                "usage": {"input_tokens": 1000, "output_tokens": 100}}})),
    ]
    .concat()
}

fn subagent(temp: &Temp, agent: &str, tool_use_id: &str, at: &str) {
    temp.write(
        &format!("s-alpha/subagents/agent-{agent}.meta.json"),
        &serde_json::json!({"agentType": "implementer", "description": "Lorem ipsum",
            "toolUseId": tool_use_id})
        .to_string(),
    );
    temp.write(
        &format!("s-alpha/subagents/agent-{agent}.jsonl"),
        &line(
            serde_json::json!({"type": "assistant", "uuid": format!("{agent}-1"),
            "timestamp": at, "requestId": format!("req-{agent}"),
            "message": {"id": format!("m-{agent}"), "model": "claude-haiku-4-5",
                "content": [], "usage": {"input_tokens": 10, "output_tokens": 1}}}),
        ),
    );
}

/// A registry command listing s-alpha as busy; counts its runs.
fn registry(runs: Arc<AtomicUsize>) -> Runner {
    Box::new(move || {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(
            br#"[{"pid":4101,"cwd":"/work/alpha","kind":"interactive","startedAt":1791000000000,
                "sessionId":"s-alpha","name":"alpha-1","status":"busy"}]"#
                .to_vec(),
        )
    })
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn counter() -> (Arc<AtomicUsize>, impl FnMut() + Send + 'static) {
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    (count, move || {
        c.fetch_add(1, Ordering::SeqCst);
    })
}

#[test]
fn it_indexes_then_goes_live_with_registry_and_discovered_edges() {
    let temp = Temp::new("live");
    temp.write("s-alpha.jsonl", &session_lines());
    subagent(&temp, "a1", "toolu_1", "2026-10-08T10:00:20.000Z");
    let (changes, on_change) = counter();
    let runs = Arc::new(AtomicUsize::new(0));
    let engine =
        Engine::start(temp.config(Some(registry(runs.clone())), None), on_change).expect("start");
    let progress = engine.progress();

    wait_for("live", || progress.status() == Status::Live);
    wait_for("a change", || changes.load(Ordering::SeqCst) > 0);
    let reader = Store::open_reader(temp.db()).expect("reader");
    wait_for("the dispatch edge", || {
        let rows = views::workstreams(&reader).expect("workstreams");
        rows.first()
            .and_then(|w| views::live_graph(&reader, w.id).expect("graph"))
            .is_some_and(|g| g.live && g.edges.len() == 1)
    });
    let rows = views::workstreams(&reader).expect("workstreams");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "alpha-1");
    assert_eq!(rows[0].state, AgentState::Working);
    assert!(runs.load(Ordering::SeqCst) > 0);
    engine.stop();
}

#[test]
fn a_subagent_dispatched_after_catch_up_gets_its_edge() {
    let temp = Temp::new("late-subagent");
    temp.write("s-alpha.jsonl", &session_lines());
    let (_changes, on_change) = counter();
    let engine = Engine::start(temp.config(None, None), on_change).expect("start");
    let progress = engine.progress();
    wait_for("live", || progress.status() == Status::Live);

    subagent(&temp, "a2", "toolu_1", "2026-10-08T10:00:30.000Z");
    let reader = Store::open_reader(temp.db()).expect("reader");
    wait_for("a2's edge", || {
        let rows = views::workstreams(&reader).expect("workstreams");
        rows.first()
            .and_then(|w| views::live_graph(&reader, w.id).expect("graph"))
            .is_some_and(|g| {
                g.agents
                    .iter()
                    .any(|a| a.agent_id.as_deref() == Some("a2") && a.parent.is_some())
            })
    });
    engine.stop();
}

#[test]
fn a_restart_reads_no_indexed_bytes() {
    let temp = Temp::new("restart");
    temp.write("s-alpha.jsonl", &session_lines());
    let (_changes, on_change) = counter();
    let engine = Engine::start(temp.config(None, None), on_change).expect("start");
    let progress = engine.progress();
    wait_for("live", || progress.status() == Status::Live);
    assert!(progress.bytes_read() > 0);
    engine.stop();

    let (_changes, on_change) = counter();
    let engine = Engine::start(temp.config(None, None), on_change).expect("restart");
    let progress = engine.progress();
    wait_for("live again", || progress.status() == Status::Live);
    assert_eq!(progress.bytes_read(), 0);
    engine.stop();
}

#[test]
fn a_burst_of_batches_notifies_at_most_once_per_debounce() {
    let temp = Temp::new("burst");
    let session = temp.write("s-alpha.jsonl", &session_lines());
    let (changes, on_change) = counter();
    let mut config = temp.config(None, None);
    config.debounce = Duration::from_millis(300);
    config.tail.batch_records = 1;
    let engine = Engine::start(config, on_change).expect("start");
    let progress = engine.progress();
    wait_for("live", || progress.status() == Status::Live);
    thread::sleep(Duration::from_millis(400));

    let before = changes.load(Ordering::SeqCst);
    let started = Instant::now();
    let mut file = OpenOptions::new()
        .append(true)
        .open(&session)
        .expect("append");
    for n in 0..200 {
        let record = serde_json::json!({"type": "user", "uuid": format!("burst-{n}"),
            "timestamp": "2026-10-08T10:01:00.000Z", "message": {"content": "Lorem."}});
        file.write_all(line(record).as_bytes()).expect("write");
        file.flush().expect("flush");
        thread::sleep(Duration::from_millis(5));
    }
    let reader = Store::open_reader(temp.db()).expect("reader");
    wait_for("the burst stored", || {
        reader
            .query_row(
                "SELECT count(*) FROM events WHERE uuid LIKE 'burst-%'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .expect("count")
            == 200
    });
    // The trailing change lands one debounce later at most.
    thread::sleep(Duration::from_millis(400));
    let notified = changes.load(Ordering::SeqCst) - before;
    let windows = started.elapsed().as_millis() / 300 + 1;
    assert!(notified >= 1, "the burst was never notified");
    assert!(
        notified as u128 <= windows,
        "{notified} notifications in {windows} windows"
    );
    engine.stop();
}

fn post(addr: SocketAddr, body: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write!(
        stream,
        "POST /hooks HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .expect("send");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("answer");
    answer
}

#[test]
fn hooks_are_off_unless_configured() {
    let temp = Temp::new("hooks-off");
    let (_changes, on_change) = counter();
    let engine = Engine::start(temp.config(None, None), on_change).expect("start");
    assert_eq!(engine.hook_addr(), None);
    engine.stop();
}

#[test]
fn an_opted_in_hook_receiver_feeds_the_store_and_stop_ends_everything() {
    let temp = Temp::new("hooks-on");
    let session = temp.write("s-alpha.jsonl", &session_lines());
    let (changes, on_change) = counter();
    let runs = Arc::new(AtomicUsize::new(0));
    let hooks = HookOptions::default();
    let engine = Engine::start(
        temp.config(Some(registry(runs.clone())), Some(hooks)),
        on_change,
    )
    .expect("start");
    let addr = engine.hook_addr().expect("receiver on");
    assert!(addr.ip().is_loopback());
    let progress = engine.progress();
    wait_for("live", || progress.status() == Status::Live);

    let answer = post(
        addr,
        r#"{"hook_event_name":"PermissionRequest","session_id":"s-alpha","agent_id":"a9","tool_name":"Bash"}"#,
    );
    assert!(answer.starts_with("HTTP/1.1 204"), "{answer}");
    let reader = Store::open_reader(temp.db()).expect("reader");
    wait_for("needs-you", || {
        views::needs_you(&reader)
            .expect("needs_you")
            .iter()
            .any(|n| n.source == "hook")
    });
    wait_for("a change", || changes.load(Ordering::SeqCst) > 0);

    engine.stop();
    // The receiver's port is closed, the poller no longer runs and appends
    // are no longer ingested.
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err());
    let runs_at_stop = runs.load(Ordering::SeqCst);
    let events = |reader: &rusqlite::Connection| -> i64 {
        reader
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .expect("count")
    };
    let stored = events(&reader);
    fs::OpenOptions::new()
        .append(true)
        .open(&session)
        .expect("append")
        .write_all(
            line(serde_json::json!({"type": "user", "uuid": "after-stop",
                "message": {"content": "Lorem."}}))
            .as_bytes(),
        )
        .expect("write");
    thread::sleep(Duration::from_millis(400));
    assert_eq!(runs.load(Ordering::SeqCst), runs_at_stop);
    assert_eq!(events(&reader), stored);
}
