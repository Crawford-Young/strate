//! Integration tests for the threaded tailer over synthetic transcripts
//! written to temp dirs at test time (nothing here is committed).

use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use strate_core::store::Store;
use strate_core::tail::{Batch, Offsets, Receiver, ResetReason, TailEvent, TailOptions, Tailer};

const PATIENCE: Duration = Duration::from_secs(10);

/// A fresh config-dir root (plus a sibling dir for stores), removed on drop.
struct Root {
    base: PathBuf,
}

impl Root {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!("strate-tail-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("claude")).expect("root");
        fs::create_dir_all(base.join("state")).expect("state dir");
        Self { base }
    }

    fn path(&self) -> PathBuf {
        self.base.join("claude")
    }

    fn store_file(&self) -> PathBuf {
        self.base.join("state/strate.db")
    }

    /// Writes `text` to `rel` under the root, creating parent dirs.
    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.path().join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, text).expect("write");
        path
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

const SESSION: &str = "projects/-work-demo/s1.jsonl";
const SUBAGENT: &str = "projects/-work-demo/s1/subagents/agent-a1.jsonl";

fn lines(range: std::ops::Range<i64>) -> String {
    range.map(|n| format!("{{\"n\":{n}}}\n")).collect()
}

fn append(path: &Path, text: &str) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open for append")
        .write_all(text.as_bytes())
        .expect("append");
}

/// Options where only `notify` can deliver a change after the first pass.
fn notify_only() -> TailOptions {
    TailOptions {
        rescan_every: Duration::from_secs(3600),
        ..TailOptions::default()
    }
}

/// Receives batches until `done` holds for the events so far, handing each
/// batch to `on_batch` first.
fn collect_until(
    rx: &Receiver<Batch>,
    mut on_batch: impl FnMut(&Batch),
    done: impl Fn(&[TailEvent]) -> bool,
) -> Vec<TailEvent> {
    let deadline = Instant::now() + PATIENCE;
    let mut events = Vec::new();
    while !done(&events) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(batch) => {
                on_batch(&batch);
                events.extend(batch.events);
            }
            Err(e) => panic!("{e}; got so far: {events:?}"),
        }
    }
    events
}

/// `value["n"]` of every record for `path`, in delivery order.
fn ns(events: &[TailEvent], path: &Path) -> Vec<i64> {
    events
        .iter()
        .filter_map(|e| match e {
            TailEvent::Record { path: p, value, .. } if **p == *path => value["n"].as_i64(),
            _ => None,
        })
        .collect()
}

fn record_count(events: &[TailEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, TailEvent::Record { .. }))
        .count()
}

#[test]
fn restart_reads_only_the_appended_bytes() {
    let root = Root::new("restart");
    let session = root.write(SESSION, &lines(0..3));
    let subagent = root.write(SUBAGENT, &lines(10..12));
    let indexed =
        fs::metadata(&session).expect("meta").len() + fs::metadata(&subagent).expect("meta").len();

    // The consumer stores each batch's events and checkpoint together; the
    // restart resumes from the store's committed offsets.
    let mut store = Store::open(root.store_file()).expect("store");
    let offsets = store.offsets().expect("offsets");
    let (tailer, rx) = Tailer::start(root.path(), offsets, notify_only()).expect("start");
    let ingest = |store: &mut Store, b: &Batch| store.ingest(b).expect("ingest");
    let events = collect_until(&rx, |b| ingest(&mut store, b), |e| record_count(e) == 5);
    assert_eq!(ns(&events, &session), vec![0, 1, 2]);
    assert_eq!(ns(&events, &subagent), vec![10, 11]);
    assert_eq!(tailer.bytes_read(), indexed);
    drop(tailer);
    drop(rx);
    drop(store);

    let appended = lines(3..5);
    append(&session, &appended);

    let mut store = Store::open(root.store_file()).expect("reopen store");
    let offsets = store.offsets().expect("offsets");
    let (tailer, rx) = Tailer::start(root.path(), offsets, notify_only()).expect("restart");
    let events = collect_until(&rx, |b| ingest(&mut store, b), |e| record_count(e) == 2);
    assert_eq!(ns(&events, &session), vec![3, 4]);
    assert_eq!(tailer.bytes_read(), appended.len() as u64);
    tailer.stop();
    let late: Vec<Batch> = rx.try_iter().collect();
    assert!(late.is_empty(), "nothing else emitted: {late:?}");
}

/// Folds each batch's checkpoint into `offsets`, as a consumer would store it.
fn track(offsets: &mut Offsets) -> impl FnMut(&Batch) + '_ {
    |b| {
        if let Some(c) = b.checkpoint {
            offsets.insert(b.path.to_path_buf(), c);
        }
    }
}

#[test]
fn live_appends_and_new_transcripts_arrive_via_notify() {
    let root = Root::new("live");
    let session = root.write(SESSION, &lines(0..1));
    let (tailer, rx) = Tailer::start(root.path(), Offsets::new(), notify_only()).expect("start");
    let mut offsets = Offsets::new();
    collect_until(&rx, track(&mut offsets), |e| record_count(e) == 1);

    // A line written in two appends, as Claude Code does mid-line.
    append(&session, "{\"n\":");
    append(&session, "1}\n");
    let events = collect_until(&rx, track(&mut offsets), |e| record_count(e) == 1);
    assert_eq!(ns(&events, &session), vec![1]);
    let TailEvent::Record { offset, .. } = &events[0] else {
        panic!("a record: {events:?}");
    };
    assert_eq!(*offset, 8);

    let subagent = root.write(SUBAGENT, &lines(20..22));
    let events = collect_until(&rx, track(&mut offsets), |e| record_count(e) == 2);
    assert_eq!(ns(&events, &subagent), vec![20, 21]);

    tailer.stop();
    // Two 9-byte lines (`{"n":20}\n`).
    assert_eq!(offsets.get(&subagent).map(|c| c.offset), Some(18));
}

#[test]
fn truncation_between_runs_is_surfaced_and_re_read() {
    let root = Root::new("truncate");
    let session = root.write(SESSION, &lines(0..3));
    let (tailer, rx) = Tailer::start(root.path(), Offsets::new(), notify_only()).expect("start");
    let mut offsets = Offsets::new();
    collect_until(&rx, track(&mut offsets), |e| record_count(e) == 3);
    tailer.stop();

    fs::write(&session, lines(7..8)).expect("truncate");
    let (_tailer, rx) = Tailer::start(root.path(), offsets, notify_only()).expect("restart");
    let events = collect_until(&rx, |_| {}, |e| record_count(e) == 1);
    assert_eq!(
        events[0],
        TailEvent::Reset {
            path: session.clone().into(),
            reason: ResetReason::Truncated,
        }
    );
    assert_eq!(ns(&events, &session), vec![7]);
}

#[test]
fn projects_dir_created_after_start_is_found_by_the_rescan() {
    let root = Root::new("late-projects");
    let options = TailOptions {
        rescan_every: Duration::from_millis(50),
        ..TailOptions::default()
    };
    let (_tailer, rx) = Tailer::start(root.path(), Offsets::new(), options).expect("start");
    let session = root.write(SESSION, &lines(0..2));
    let events = collect_until(&rx, |_| {}, |e| record_count(e) == 2);
    assert_eq!(ns(&events, &session), vec![0, 1]);

    append(&session, &lines(2..3));
    let events = collect_until(&rx, |_| {}, |e| record_count(e) == 1);
    assert_eq!(ns(&events, &session), vec![2]);
}

#[test]
fn only_transcripts_are_read_never_credentials_or_sessions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home");
    assert!(root.join(".credentials.json").exists());
    assert!(root.join("sessions/123.json").exists());
    // Independent oracle: total size of the transcript files, found by
    // walking the fixture rather than through the crate.
    let mut transcripts = Vec::new();
    walk(&root.join("projects"), &mut transcripts);
    transcripts.retain(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        name.ends_with(".jsonl") && !p.components().any(|c| c.as_os_str() == "tool-results")
    });
    let total: u64 = transcripts
        .iter()
        .map(|p| fs::metadata(p).expect("meta").len())
        .sum();

    let (tailer, rx) = Tailer::start(&root, Offsets::new(), notify_only()).expect("start");
    let deadline = Instant::now() + PATIENCE;
    let mut events = Vec::new();
    while tailer.bytes_read() < total && Instant::now() < deadline {
        if let Ok(batch) = rx.recv_timeout(Duration::from_millis(50)) {
            events.extend(batch.events);
        }
    }
    tailer.stop();
    events.extend(rx.try_iter().flat_map(|b| b.events));

    assert!(!events.is_empty());
    for e in &events {
        let (TailEvent::Record { path, .. }
        | TailEvent::Malformed { path, .. }
        | TailEvent::Reset { path, .. }
        | TailEvent::Error { path, .. }) = e;
        assert!(transcripts.iter().any(|t| **path == *t), "{e:?}");
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("readable dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn stop_returns_even_while_the_consumer_is_not_reading() {
    let root = Root::new("stop");
    root.write(SESSION, &lines(0..5000));
    let options = TailOptions {
        batch_records: 1,
        channel_batches: 1,
        ..notify_only()
    };
    let (tailer, rx) = Tailer::start(root.path(), Offsets::new(), options).expect("start");
    let deadline = Instant::now() + PATIENCE;
    while rx.is_empty() && Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(rx.len(), 1, "the channel filled up");

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        tailer.stop();
        done_tx.send(()).expect("report");
    });
    done_rx
        .recv_timeout(PATIENCE)
        .expect("stop() returned while the channel was full");
}

/// Writes ~`target` bytes of synthetic assistant records; returns the line
/// count.
fn write_synthetic(path: &Path, target: u64) -> u64 {
    fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    let mut out = BufWriter::new(fs::File::create(path).expect("create"));
    let pad = "lorem ipsum dolor sit amet ".repeat(36);
    let (mut written, mut n) = (0u64, 0u64);
    while written < target {
        let line = format!(
            "{{\"type\":\"assistant\",\"uuid\":\"00000000-0000-4000-8000-{n:012}\",\"n\":{n},\"cwd\":\"/work/demo\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{pad}\"}}]}}}}\n"
        );
        out.write_all(line.as_bytes()).expect("write");
        written += line.len() as u64;
        n += 1;
    }
    out.flush().expect("flush");
    n
}

#[test]
fn fifty_megabytes_ingest_in_the_background_with_bounded_buffering() {
    let root = Root::new("fifty-mb");
    let session = root.path().join(SESSION);
    let line_count = write_synthetic(&session, 50 * 1024 * 1024);
    let size = fs::metadata(&session).expect("meta").len();
    assert!(
        size >= 50 * 1024 * 1024 && line_count > 40_000,
        "{size} bytes, {line_count} lines"
    );

    let started = Instant::now();
    let (tailer, rx) = Tailer::start(root.path(), Offsets::new(), notify_only()).expect("start");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "start() blocked for {:?}",
        started.elapsed()
    );

    // The consumer is busy elsewhere: the worker must stall on the full
    // channel instead of reading the whole file into memory.
    thread::sleep(Duration::from_millis(300));
    let read_while_stalled = tailer.bytes_read();
    assert!(
        read_while_stalled < size / 4,
        "read {read_while_stalled} of {size} unconsumed"
    );

    let deadline = Instant::now() + Duration::from_secs(120);
    let (mut records, mut expected_n, mut idle_turns) = (0u64, 0i64, 0u64);
    while records < line_count {
        assert!(
            Instant::now() < deadline,
            "only {records} of {line_count} records"
        );
        match rx.try_recv() {
            Ok(batch) => {
                for e in batch.events {
                    let TailEvent::Record { value, .. } = e else {
                        panic!("unexpected {e:?}");
                    };
                    assert_eq!(value["n"].as_i64(), Some(expected_n), "in order");
                    expected_n += 1;
                    records += 1;
                }
            }
            // Nothing ready: this thread is free for other work.
            Err(_) => {
                idle_turns += 1;
                thread::yield_now();
            }
        }
    }
    assert_eq!(tailer.bytes_read(), size);
    assert!(
        idle_turns > 0,
        "the consumer never had to wait, so never proved it was free"
    );
}
