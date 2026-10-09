//! The live agent registry: `claude agents --json --all`, polled.
//!
//! [`RegistryPoller::start`] returns at once. A worker thread runs the
//! command every [`PollOptions::every`], parses the output at the boundary
//! ([`parse_snapshot`]), and emits what changed since the last good poll as
//! [`RegistryEvent`]s over a bounded channel. A failed or malformed poll is
//! an [`RegistryEvent::Error`] and keeps the last snapshot. The command
//! runner is injected, so tests never spawn the real CLI. The registry
//! lists sessions, never subagents (those arrive through the tail first).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Read};
use std::panic::resume_unwind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded, select, tick};
use serde::Deserialize;

/// One entry of `claude agents --json --all`. Every entry has `cwd`, `kind`
/// and `startedAt`; a background session adds `id` and `state`, a live one
/// `pid` and `status`, a waiting one `waitingFor`; `sessionId` and `name`
/// appear when set. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    pub cwd: String,
    pub kind: Kind,
    /// Unix ms.
    pub started_at: i64,
    pub pid: Option<u32>,
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub status: Option<Status>,
    pub waiting_for: Option<String>,
    /// A background session's state, kept as the CLI spells it.
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum Kind {
    Interactive,
    Background,
    /// A kind this build does not know, kept verbatim.
    Other(String),
}

/// A live session's status. Needs-you is [`Status::Waiting`] with a
/// `waitingFor`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum Status {
    Busy,
    Waiting,
    Idle,
    /// A status this build does not know, kept verbatim.
    Other(String),
}

impl From<String> for Kind {
    fn from(s: String) -> Self {
        match s.as_str() {
            "interactive" => Self::Interactive,
            "background" => Self::Background,
            _ => Self::Other(s),
        }
    }
}

impl Kind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
            Self::Other(s) => s,
        }
    }
}

impl From<String> for Status {
    fn from(s: String) -> Self {
        match s.as_str() {
            "busy" => Self::Busy,
            "waiting" => Self::Waiting,
            "idle" => Self::Idle,
            _ => Self::Other(s),
        }
    }
}

impl Status {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Busy => "busy",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Other(s) => s,
        }
    }
}

impl RegistryEntry {
    /// The entry's identity across polls: a background `id`, else the
    /// process (`pid` with its start, as pids are reused), else the
    /// `sessionId`, else its start and cwd.
    pub fn key(&self) -> String {
        match (&self.id, self.pid, &self.session_id) {
            (Some(id), _, _) => format!("id:{id}"),
            (None, Some(pid), _) => format!("pid:{pid}:{}", self.started_at),
            (None, None, Some(session)) => format!("session:{session}"),
            (None, None, None) => format!("start:{}:{}", self.started_at, self.cwd),
        }
    }
}

/// Parses one poll's output: a JSON array of entries.
pub fn parse_snapshot(bytes: &[u8]) -> serde_json::Result<Vec<RegistryEntry>> {
    serde_json::from_slice(bytes)
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegistryEvent {
    /// A new entry, first seen by the poll at `at_ms` (unix ms).
    Appeared { entry: RegistryEntry, at_ms: i64 },
    /// An entry whose fields moved; `entry` is the new value.
    Changed { entry: RegistryEntry, at_ms: i64 },
    /// An entry no longer listed; `last_seen_ms` is the last poll that
    /// listed it.
    Gone {
        entry: RegistryEntry,
        last_seen_ms: i64,
    },
    /// The command failed or printed something unparseable.
    Error { message: String, at_ms: i64 },
}

/// Runs the registry command once and returns its stdout.
pub type Runner = Box<dyn FnMut() -> io::Result<Vec<u8>> + Send>;

#[derive(Debug, Clone)]
pub struct PollOptions {
    pub every: Duration,
    /// Events buffered before the worker waits for the consumer.
    pub channel_events: usize,
}

impl Default for PollOptions {
    fn default() -> Self {
        Self {
            every: Duration::from_secs(1),
            channel_events: 256,
        }
    }
}

/// The Claude Code binary: `explicit` when given, else `claude` on `path`
/// (the `PATH` value), else `<home>/.local/bin/claude`. Pure: the caller
/// passes the env, so nothing here reads the process environment.
pub fn resolve_claude(
    explicit: Option<PathBuf>,
    path: Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    const NAMES: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    };
    if explicit.is_some() {
        return explicit;
    }
    let dirs = path.iter().flat_map(std::env::split_paths);
    let local = home.map(|h| h.join(".local").join("bin"));
    dirs.chain(local)
        .flat_map(|dir| NAMES.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Longest a single `claude agents` run may take before it is killed.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .args(["agents", "--json", "--all"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up once a second.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// A [`Runner`] that spawns `program agents --json --all`, killing a run
/// that outlasts 10 s. A non-zero exit is an error.
pub fn cli_runner(program: PathBuf) -> Runner {
    Box::new(move || run(command(&program), RUN_TIMEOUT))
}

fn run(mut command: Command, timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    // Drained on its own thread so a full pipe cannot stall the child.
    let reader = thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no answer within {timeout:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let out = reader.join().unwrap_or_else(|panic| resume_unwind(panic))?;
    if status.success() {
        Ok(out)
    } else {
        Err(io::Error::other(format!("exited with {status}")))
    }
}

/// A running poller. Dropping it stops the worker, as
/// [`RegistryPoller::stop`] does.
pub struct RegistryPoller {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl RegistryPoller {
    /// Starts polling on a worker thread (the first poll at once) and
    /// returns the receiving end of the event channel.
    pub fn start(
        runner: Runner,
        options: PollOptions,
    ) -> io::Result<(Self, Receiver<RegistryEvent>)> {
        let (out, events) = bounded(options.channel_events);
        let (stop, stop_rx) = bounded(0);
        let worker = thread::Builder::new()
            .name("strate-registry".into())
            .spawn(move || poll_loop(runner, options.every, &out, &stop_rx))?;
        let poller = Self {
            stop: Some(stop),
            worker: Some(worker),
        };
        Ok((poller, events))
    }

    /// Stops the worker once any run in flight ends. Returns promptly even
    /// when the channel is full; events already queued stay receivable.
    pub fn stop(mut self) {
        self.stop.take();
        let worker = self.worker.take().expect("worker runs until stop");
        worker.join().unwrap_or_else(|panic| resume_unwind(panic));
    }
}

impl Drop for RegistryPoller {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            let _panicked = worker.join();
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

struct Stopped;

/// Runs until stopped or the consumer hangs up.
fn poll_loop(
    mut runner: Runner,
    every: Duration,
    out: &Sender<RegistryEvent>,
    stop: &Receiver<()>,
) {
    let send = |event| {
        select! {
            send(out, event) -> sent => sent.map_err(|_| Stopped),
            recv(stop) -> _ => Err(Stopped),
        }
    };
    let mut last: BTreeMap<String, RegistryEntry> = BTreeMap::new();
    let mut last_ms = 0;
    let ticker = tick(every);
    loop {
        let at_ms = now_ms();
        let polled = runner()
            .map_err(|e| e.to_string())
            .and_then(|bytes| parse_snapshot(&bytes).map_err(|e| e.to_string()));
        let events = match polled {
            Ok(entries) => {
                let next: BTreeMap<_, _> = entries.into_iter().map(|e| (e.key(), e)).collect();
                let events = diff(&last, &next, at_ms, last_ms);
                (last, last_ms) = (next, at_ms);
                events
            }
            Err(message) => vec![RegistryEvent::Error { message, at_ms }],
        };
        for event in events {
            if send(event).is_err() {
                return;
            }
        }
        select! {
            recv(stop) -> _ => return,
            recv(ticker) -> _ => {}
        }
    }
}

fn diff(
    last: &BTreeMap<String, RegistryEntry>,
    next: &BTreeMap<String, RegistryEntry>,
    at_ms: i64,
    last_ms: i64,
) -> Vec<RegistryEvent> {
    let mut events = Vec::new();
    for (key, entry) in next {
        match last.get(key) {
            None => events.push(RegistryEvent::Appeared {
                entry: entry.clone(),
                at_ms,
            }),
            Some(before) if before != entry => events.push(RegistryEvent::Changed {
                entry: entry.clone(),
                at_ms,
            }),
            Some(_) => {}
        }
    }
    for (key, entry) in last {
        if !next.contains_key(key) {
            events.push(RegistryEvent::Gone {
                entry: entry.clone(),
                last_seen_ms: last_ms,
            });
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_command_asks_for_every_entry_as_json() {
        let command = command(Path::new("claude"));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["agents", "--json", "--all"]);
    }

    #[test]
    fn a_missing_binary_is_an_error() {
        let mut runner = cli_runner(PathBuf::from("/no/such/dir/claude-strate-test"));
        assert!(runner().is_err());
    }

    /// The current test binary, run with a filter matching no test: it
    /// prints libtest's summary and exits 0.
    fn this_binary(filter: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("exe"));
        command
            .args([filter, "--exact"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn a_run_returns_stdout_and_fails_on_a_non_zero_exit() {
        let out = run(this_binary("no-such-test"), RUN_TIMEOUT).expect("runs");
        assert!(String::from_utf8_lossy(&out).contains("0 passed"));

        let mut failing = this_binary("no-such-test");
        failing.arg("--no-such-flag");
        assert!(run(failing, RUN_TIMEOUT).is_err());
    }

    #[test]
    fn a_run_past_the_timeout_is_killed() {
        let mut slow = this_binary("registry::tests::sleeper");
        slow.env("STRATE_SLEEPER", "1");
        let started = Instant::now();
        let err = run(slow, Duration::from_millis(200)).expect_err("killed");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Sleeps only when run by [`a_run_past_the_timeout_is_killed`].
    #[test]
    fn sleeper() {
        if std::env::var_os("STRATE_SLEEPER").is_some() {
            thread::sleep(Duration::from_secs(30));
        }
    }
}
