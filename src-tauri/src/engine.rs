//! The ingest engine: one background thread that owns the [`Store`] writer
//! and feeds it from the tailer, the registry poller, on-request GitHub
//! fetches and, when opted in, the hook receiver.
//!
//! rusqlite connections are not `Sync`, so the writer never leaves this
//! thread; commands read through their own read-only connection, which WAL
//! lets query the last commit while a write is open. Nothing here runs on
//! the UI thread: [`Engine::start`] only opens the store, starts the
//! sources and spawns the thread.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded, never, select, unbounded};
use serde::Serialize;
use strate_core::discovery::{Link, discover};
use strate_core::github::{self, Fetched, Target};
use strate_core::hooks::{Hook, HookOptions, HookReceiver};
use strate_core::registry::{PollOptions, RegistryEvent, RegistryPoller, Runner};
use strate_core::store::Store;
use strate_core::tail::{Batch, TailEvent, TailOptions, Tailer};

/// Discovery passes a subagent transcript may ask for before its link is
/// given up on (no `.meta.json`, or a `toolUseId` with no `tool_use`).
const DISCOVERY_TRIES: u8 = 3;

/// Longest the thread sleeps with nothing pending.
const IDLE: Duration = Duration::from_secs(3600);

pub struct Config {
    /// The Claude Code config dir.
    pub root: PathBuf,
    /// The SQLite store.
    pub db: PathBuf,
    /// Runs `claude agents --json --all`; `None` leaves the registry off.
    pub registry: Option<Runner>,
    /// Runs `gh` for [`Refresher::request`]; `None` leaves GitHub off, so a
    /// request is done at once.
    pub github: Option<github::Runner>,
    pub poll: PollOptions,
    /// `Some` starts the hook receiver (opt-in).
    pub hooks: Option<HookOptions>,
    pub tail: TailOptions,
    /// Shortest gap between two change notifications.
    pub debounce: Duration,
    /// Shortest gap between two discovery passes.
    pub rediscover_after: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The initial scan is still running.
    Indexing,
    /// Caught up; changes arrive as they happen.
    Live,
}

/// What the engine thread publishes for commands to read.
#[derive(Debug, Default)]
pub struct Progress {
    live: AtomicBool,
    bytes_read: AtomicU64,
}

impl Progress {
    pub fn status(&self) -> Status {
        if self.live.load(Ordering::Acquire) {
            Status::Live
        } else {
            Status::Indexing
        }
    }

    /// Transcript bytes this run's tailer has read.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }
}

/// A running engine. Dropping it stops everything, as [`Engine::stop`]
/// does.
pub struct Engine {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
    progress: Arc<Progress>,
    refresher: Refresher,
    hook_addr: Option<SocketAddr>,
}

/// Asks the engine to refresh the GitHub cache. Cheap to clone and safe to
/// call from any thread.
#[derive(Debug, Clone)]
pub struct Refresher(Sender<Waiter>);

/// Told once the fetch that covers its request is stored (or none will run).
type Waiter = Sender<()>;

impl Refresher {
    /// Requests a refresh and returns at once with a receiver that gets
    /// `()`, or disconnects, once it is done: the fetch it started, or the
    /// one after the fetch it came during, is stored, or GitHub is off, has
    /// no repos to fetch or failed to list them. Requests made while a fetch
    /// runs coalesce into one more after it. One made after the engine
    /// stopped is done at once (the receiver disconnects). Nothing here can
    /// fail: GitHub trouble is stored per repo, never raised.
    pub fn request(&self) -> Receiver<()> {
        let (waiter, done) = bounded(1);
        let _stopped = self.0.send(waiter);
        done
    }

    /// [`Self::request`], then blocks until it is done.
    pub fn refresh(&self) {
        let _done_or_stopped = self.request().recv();
    }
}

impl Engine {
    /// Opens the store at `config.db` (creating it), starts the tailer from
    /// its committed offsets, the registry poller and the opted-in hook
    /// receiver, and spawns the thread that writes what they deliver.
    /// `on_change` runs on that thread after the store changed, at most
    /// once per `config.debounce`.
    pub fn start(config: Config, on_change: impl FnMut() + Send + 'static) -> io::Result<Self> {
        let root = std::path::absolute(&config.root)?;
        let store = Store::open(&config.db).map_err(io::Error::other)?;
        let offsets = store.offsets().map_err(io::Error::other)?;
        let (tailer, batches) = Tailer::start(&root, offsets, config.tail)?;
        let (poller, registry) = match config.registry {
            Some(runner) => {
                let (poller, events) = RegistryPoller::start(runner, config.poll)?;
                (Some(poller), events)
            }
            None => (None, never()),
        };
        let (receiver, hooks) = match config.hooks {
            Some(options) => {
                let (receiver, hooks) = HookReceiver::start(options)?;
                (Some(receiver), hooks)
            }
            None => (None, never()),
        };
        let (fetcher, fetched) = match config.github {
            Some(runner) => {
                let (jobs, fetched) = spawn_fetcher(runner)?;
                (Some(jobs), fetched)
            }
            None => (None, never()),
        };
        let (request, refresh) = unbounded();
        let hook_addr = receiver.as_ref().map(HookReceiver::local_addr);
        let progress = Arc::new(Progress::default());
        let (stop, stop_rx) = bounded(0);
        let ingest = Ingest {
            root,
            store,
            progress: progress.clone(),
            notify: Throttle::new(config.debounce),
            discovery: Discovery::new(config.rediscover_after),
            fetcher,
            fetching: false,
            pending: false,
            waiters: Vec::new(),
            on_change,
        };
        let sources = Sources {
            tailer,
            poller,
            receiver,
            batches,
            registry,
            hooks,
            refresh,
            fetched,
            stop: stop_rx,
        };
        let worker = thread::Builder::new()
            .name("strate-ingest".into())
            .spawn(move || ingest.run(sources))?;
        Ok(Self {
            stop: Some(stop),
            worker: Some(worker),
            progress,
            refresher: Refresher(request),
            hook_addr,
        })
    }

    pub fn progress(&self) -> Arc<Progress> {
        self.progress.clone()
    }

    /// The handle that requests a GitHub refresh.
    pub fn refresher(&self) -> Refresher {
        self.refresher.clone()
    }

    /// Where the hook receiver listens; `None` when it is off.
    pub fn hook_addr(&self) -> Option<SocketAddr> {
        self.hook_addr
    }

    /// Stops the poller, the hook receiver and the tailer, then the engine
    /// thread, once any write in flight commits.
    pub fn stop(mut self) {
        self.stop.take();
        let worker = self.worker.take().expect("worker runs until stop");
        worker.join().unwrap_or_else(|panic| resume_unwind(panic));
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            let _panicked = worker.join();
        }
    }
}

struct Sources {
    tailer: Tailer,
    poller: Option<RegistryPoller>,
    receiver: Option<HookReceiver>,
    batches: Receiver<Batch>,
    registry: Receiver<RegistryEvent>,
    hooks: Receiver<Hook>,
    refresh: Receiver<Waiter>,
    fetched: Receiver<Vec<Fetched>>,
    stop: Receiver<()>,
}

/// The fetch thread's ends: jobs in, fetched repos out.
type Fetcher = (Sender<Vec<Target>>, Receiver<Vec<Fetched>>);

/// Runs `runner` on its own thread, so a slow `gh` never holds up the
/// engine thread. Jobs go in one at a time (the engine sends none while one
/// is out). The thread ends when the engine drops the jobs channel, after
/// the fetch in flight; it is not joined, so stopping never waits on `gh`.
fn spawn_fetcher(mut runner: github::Runner) -> io::Result<Fetcher> {
    let (jobs, queue) = bounded::<Vec<Target>>(1);
    let (done, fetched) = bounded(1);
    thread::Builder::new()
        .name("strate-github".into())
        .spawn(move || {
            for targets in queue {
                if done.send(fetch_guarded(&mut runner, &targets)).is_err() {
                    break;
                }
            }
        })?;
    Ok((jobs, fetched))
}

/// [`github::fetch_all`], with a panic failing every target instead of
/// ending the fetch thread.
fn fetch_guarded(runner: &mut github::Runner, targets: &[Target]) -> Vec<Fetched> {
    catch_unwind(AssertUnwindSafe(|| github::fetch_all(runner, targets))).unwrap_or_else(|_| {
        targets
            .iter()
            .map(|t| Fetched::Failed {
                slug: t.slug.clone(),
                owner: t.owner.clone(),
                error: "the GitHub fetch panicked".into(),
            })
            .collect()
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Sources {
    fn stop(self) {
        if let Some(poller) = self.poller {
            poller.stop();
        }
        if let Some(receiver) = self.receiver {
            receiver.stop();
        }
        self.tailer.stop();
    }
}

/// At most one notification per `every`, sent as soon as one is due:
/// the first change notifies at once, a burst after it once per window.
struct Throttle {
    every: Duration,
    last: Option<Instant>,
    dirty: bool,
}

impl Throttle {
    fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            dirty: false,
        }
    }

    fn mark(&mut self) {
        self.dirty = true;
    }

    /// Whether to notify at `now`; a notification clears the mark.
    fn take(&mut self, now: Instant) -> bool {
        let due = self.dirty && self.last.is_none_or(|last| now >= last + self.every);
        if due {
            self.dirty = false;
            self.last = Some(now);
        }
        due
    }

    /// How long until a marked change is due; `None` when nothing is.
    fn wait(&self, now: Instant) -> Option<Duration> {
        self.dirty.then(|| {
            self.last.map_or(Duration::ZERO, |last| {
                (last + self.every).saturating_duration_since(now)
            })
        })
    }
}

/// When to run discovery, which supplies subagent meta and the dispatch
/// and teammate edges that transcripts alone do not carry: once after the
/// initial scan, then when a subagent transcript it has not linked yet
/// gets records, a few tries per transcript, no more often than `after`.
struct Discovery {
    after: Duration,
    last: Option<Instant>,
    due: bool,
    linked: HashSet<PathBuf>,
    tries: HashMap<PathBuf, u8>,
}

impl Discovery {
    fn new(after: Duration) -> Self {
        Self {
            after,
            last: None,
            due: false,
            linked: HashSet::new(),
            tries: HashMap::new(),
        }
    }

    /// A batch for `path` arrived after the initial scan.
    fn saw(&mut self, path: &Path) {
        let subagent = path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|dir| dir == "subagents");
        if !subagent || self.linked.contains(path) {
            return;
        }
        let tries = self.tries.entry(path.to_path_buf()).or_default();
        if *tries < DISCOVERY_TRIES {
            *tries += 1;
            self.due = true;
        }
    }

    fn wait(&self, now: Instant) -> Option<Duration> {
        self.due.then(|| {
            self.last.map_or(Duration::ZERO, |last| {
                (last + self.after).saturating_duration_since(now)
            })
        })
    }
}

struct Ingest<F> {
    root: PathBuf,
    store: Store,
    progress: Arc<Progress>,
    notify: Throttle,
    discovery: Discovery,
    /// Feeds the GitHub fetch thread; `None` with GitHub off or after the
    /// thread ended.
    fetcher: Option<Sender<Vec<Target>>>,
    /// A fetch is out.
    fetching: bool,
    /// A request came while a fetch was out: one more runs after it, so a
    /// window named meanwhile is fetched too.
    pending: bool,
    /// The requests not done yet; all are told once no fetch is out or due.
    waiters: Vec<Waiter>,
    on_change: F,
}

impl<F: FnMut()> Ingest<F> {
    fn run(mut self, sources: Sources) {
        let (mut batches, mut registry, mut hooks, mut refresh, mut fetched) = (
            sources.batches.clone(),
            sources.registry.clone(),
            sources.hooks.clone(),
            sources.refresh.clone(),
            sources.fetched.clone(),
        );
        loop {
            let now = Instant::now();
            let wait = [self.notify.wait(now), self.discovery.wait(now)]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(IDLE);
            select! {
                recv(sources.stop) -> _ => break,
                recv(batches) -> batch => match batch {
                    Ok(batch) => self.batch(&batch),
                    Err(_) => batches = never(),
                },
                recv(registry) -> event => match event {
                    Ok(event) => {
                        let events: Vec<_> = std::iter::once(event).chain(registry.try_iter()).collect();
                        self.registry(&events);
                    }
                    Err(_) => registry = never(),
                },
                recv(hooks) -> hook => match hook {
                    Ok(hook) => self.hook(&hook),
                    Err(_) => hooks = never(),
                },
                recv(refresh) -> request => match request {
                    Ok(waiter) => {
                        self.waiters.push(waiter);
                        self.waiters.extend(refresh.try_iter());
                        self.refresh();
                    }
                    Err(_) => refresh = never(),
                },
                recv(fetched) -> done => match done {
                    Ok(fetched) => self.fetched(&fetched),
                    Err(_) => {
                        self.fetcher_gone();
                        fetched = never();
                    }
                },
                default(wait) => {}
            }
            self.progress
                .bytes_read
                .store(sources.tailer.bytes_read(), Ordering::Relaxed);
            let now = Instant::now();
            if self.discovery.wait(now) == Some(Duration::ZERO) {
                self.discover(now);
            }
            if self.notify.take(Instant::now()) {
                (self.on_change)();
            }
        }
        sources.stop();
    }

    fn batch(&mut self, batch: &Batch) {
        if batch.events.contains(&TailEvent::CaughtUp) {
            self.progress.live.store(true, Ordering::Release);
            self.discovery.due = true;
            self.notify.mark();
            return;
        }
        if self.progress.live.load(Ordering::Acquire) {
            self.discovery.saw(&batch.path);
        }
        match self.store.ingest(batch) {
            Ok(()) => self.notify.mark(),
            Err(e) => eprintln!("strate: ingest failed: {e}"),
        }
    }

    fn registry(&mut self, events: &[RegistryEvent]) {
        let changes = events
            .iter()
            .any(|e| !matches!(e, RegistryEvent::Error { .. }));
        if !changes {
            return;
        }
        match self.store.apply_registry(events) {
            Ok(()) => self.notify.mark(),
            Err(e) => eprintln!("strate: registry update failed: {e}"),
        }
    }

    fn hook(&mut self, hook: &Hook) {
        match self.store.apply_hook(hook) {
            Ok(()) => self.notify.mark(),
            Err(e) => eprintln!("strate: hook update failed: {e}"),
        }
    }

    /// Hands the fetch thread the repos of the stored windows, unless
    /// GitHub is off. While a fetch is out, requests coalesce into one more
    /// after it. With no fetch out after this, the waiters are done.
    fn refresh(&mut self) {
        if self.fetching {
            self.pending = true;
            return;
        }
        self.start_fetch();
        if !self.fetching {
            self.release_waiters();
        }
    }

    fn start_fetch(&mut self) {
        let Some(fetcher) = &self.fetcher else {
            return;
        };
        let targets = self
            .store
            .window_slugs()
            .and_then(|slugs| github::targets(&self.store, &slugs));
        match targets {
            Ok(targets) if targets.is_empty() => {}
            Ok(targets) => match fetcher.send(targets) {
                Ok(()) => self.fetching = true,
                Err(_) => self.fetcher_gone(),
            },
            Err(e) => eprintln!("strate: reading the repos to fetch failed: {e}"),
        }
    }

    /// Stores a finished fetch (new rows, or the error per failed repo),
    /// then runs the one requested meanwhile, or tells the waiters.
    fn fetched(&mut self, fetched: &[Fetched]) {
        self.fetching = false;
        match github::store_fetched(&mut self.store, fetched, now_ms()) {
            Ok(()) => self.notify.mark(),
            Err(e) => eprintln!("strate: storing GitHub failed: {e}"),
        }
        if std::mem::take(&mut self.pending) {
            self.refresh();
        } else {
            self.release_waiters();
        }
    }

    /// The fetch thread ended: GitHub refresh is off from here on, and no
    /// request waits on it.
    fn fetcher_gone(&mut self) {
        if self.fetcher.take().is_some() {
            eprintln!("strate: the GitHub fetch thread ended; GitHub refresh is off");
        }
        self.fetching = false;
        self.pending = false;
        self.release_waiters();
    }

    /// Tells every waiter its request is done; one that stopped waiting is
    /// skipped.
    fn release_waiters(&mut self) {
        for waiter in self.waiters.drain(..) {
            let _stopped_waiting = waiter.try_send(());
        }
    }

    fn discover(&mut self, now: Instant) {
        self.discovery.due = false;
        self.discovery.last = Some(now);
        let graph = match discover(&self.root) {
            Ok(graph) => graph,
            Err(e) => {
                eprintln!("strate: discovery failed: {e}");
                return;
            }
        };
        self.discovery.linked = graph
            .subagents
            .iter()
            .filter(|s| matches!(s.link, Link::Dispatch { .. } | Link::Teammate { .. }))
            .map(|s| s.path.clone())
            .collect();
        match self.store.ingest_graph(&graph) {
            Ok(()) => self.notify.mark(),
            Err(e) => eprintln!("strate: storing discovery failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_as_the_webview_expects() {
        let json = |s: Status| serde_json::to_string(&s).expect("json");
        assert_eq!(json(Status::Indexing), "\"indexing\"");
        assert_eq!(json(Status::Live), "\"live\"");
        assert_eq!(Progress::default().status(), Status::Indexing);
    }

    #[test]
    fn the_throttle_notifies_at_once_then_once_per_window() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut throttle = Throttle::new(Duration::from_millis(250));
        assert!(!throttle.take(t0), "nothing marked");
        assert_eq!(throttle.wait(t0), None);

        throttle.mark();
        assert_eq!(throttle.wait(t0), Some(Duration::ZERO));
        assert!(throttle.take(t0));
        throttle.mark();
        throttle.mark();
        assert_eq!(throttle.wait(ms(100)), Some(Duration::from_millis(150)));
        assert!(!throttle.take(ms(100)));
        assert!(throttle.take(ms(250)));
        assert!(!throttle.take(ms(600)), "the mark was spent");
    }

    #[test]
    fn a_panicking_fetch_fails_each_target_instead_of_ending_the_thread() {
        let mut runner: github::Runner = Box::new(|_| panic!("lorem"));
        let targets = [
            Target {
                slug: "a".into(),
                owner: Some("o".into()),
            },
            Target {
                slug: "b".into(),
                owner: None,
            },
        ];
        let failed = |slug: &str, owner: Option<&str>| Fetched::Failed {
            slug: slug.into(),
            owner: owner.map(Into::into),
            error: "the GitHub fetch panicked".into(),
        };
        assert_eq!(
            fetch_guarded(&mut runner, &targets),
            [failed("a", Some("o")), failed("b", None)]
        );
    }

    /// An engine-thread state over an empty store (so a refresh has no repos
    /// to fetch), with GitHub on through `fetcher`.
    fn ingest(name: &str, fetcher: Option<Sender<Vec<Target>>>) -> Ingest<impl FnMut()> {
        let dir = std::env::temp_dir().join(format!("strate-ingest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        Ingest {
            root: dir.clone(),
            store: Store::open(dir.join("strate.db")).expect("store"),
            progress: Arc::default(),
            notify: Throttle::new(Duration::ZERO),
            discovery: Discovery::new(Duration::ZERO),
            fetcher,
            fetching: false,
            pending: false,
            waiters: Vec::new(),
            on_change: || {},
        }
    }

    fn waiter(ingest: &mut Ingest<impl FnMut()>) -> Receiver<()> {
        let (waiter, done) = bounded(1);
        ingest.waiters.push(waiter);
        done
    }

    #[test]
    fn a_request_is_done_at_once_when_no_fetch_runs_for_it() {
        let mut off = ingest("off", None);
        let done = waiter(&mut off);
        off.refresh();
        assert_eq!(done.try_recv(), Ok(()), "GitHub off");

        let (jobs, queue) = bounded(1);
        let mut empty = ingest("empty", Some(jobs));
        let done = waiter(&mut empty);
        empty.refresh();
        assert_eq!(done.try_recv(), Ok(()), "no repos to fetch");
        assert!(queue.is_empty(), "nothing fetched");
        assert!(!empty.fetching);
    }

    #[test]
    fn waiters_are_told_once_the_fetch_chain_covering_them_is_stored() {
        let (jobs, _queue) = bounded(1);
        let mut ingest = ingest("chain", Some(jobs));
        ingest.fetching = true;
        let first = waiter(&mut ingest);
        ingest.refresh();
        assert!(
            ingest.pending,
            "a request during a fetch runs one more after it"
        );
        let second = waiter(&mut ingest);
        ingest.refresh();
        assert!(first.try_recv().is_err() && second.try_recv().is_err());

        // The trailing fetch has no repos here, so the chain ends with this one.
        ingest.fetched(&[]);
        assert_eq!((first.try_recv(), second.try_recv()), (Ok(()), Ok(())));
        assert!(!ingest.pending && !ingest.fetching);

        ingest.fetching = true;
        let third = waiter(&mut ingest);
        ingest.fetched(&[]);
        assert_eq!(third.try_recv(), Ok(()), "no trailing fetch due");
    }

    #[test]
    fn waiters_are_told_when_the_fetch_thread_ends() {
        let (jobs, _queue) = bounded(1);
        let mut ingest = ingest("gone", Some(jobs));
        ingest.fetching = true;
        ingest.pending = true;
        let done = waiter(&mut ingest);
        let (gave_up, _) = bounded(1);
        ingest.waiters.push(gave_up);
        ingest.fetcher_gone();
        assert_eq!(done.try_recv(), Ok(()));
        assert!(ingest.fetcher.is_none() && !ingest.pending);
    }

    #[test]
    fn a_request_after_the_engine_stopped_never_hangs() {
        let (request, refresh) = unbounded();
        let refresher = Refresher(request);
        drop(refresh);
        assert!(
            refresher.request().recv().is_err(),
            "disconnected, not blocked"
        );
        refresher.refresh();

        let (request, refresh) = unbounded::<Waiter>();
        let refresher = Refresher(request);
        let done = refresher.request();
        drop(refresh);
        assert!(
            done.recv().is_err(),
            "a queued request dies with the engine"
        );
    }

    #[test]
    fn discovery_retries_an_unlinked_subagent_a_few_times_only() {
        let t0 = Instant::now();
        let mut discovery = Discovery::new(Duration::from_secs(2));
        let sub = Path::new("/cfg/projects/p/s1/subagents/agent-a1.jsonl");
        discovery.saw(Path::new("/cfg/projects/p/s1.jsonl"));
        assert_eq!(discovery.wait(t0), None, "a session transcript never asks");

        for _ in 0..DISCOVERY_TRIES {
            discovery.saw(sub);
            assert_eq!(discovery.wait(t0), Some(Duration::ZERO));
            discovery.due = false;
        }
        discovery.saw(sub);
        assert_eq!(discovery.wait(t0), None, "given up");

        let linked = Path::new("/cfg/projects/p/s1/subagents/agent-a2.jsonl");
        discovery.linked.insert(linked.to_path_buf());
        discovery.saw(linked);
        assert_eq!(discovery.wait(t0), None, "already linked");

        discovery.due = true;
        discovery.last = Some(t0);
        assert_eq!(
            discovery.wait(t0 + Duration::from_millis(500)),
            Some(Duration::from_millis(1500))
        );
    }
}
