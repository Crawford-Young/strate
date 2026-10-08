//! Incremental tail of every transcript under a config dir.
//!
//! [`Tailer::start`] returns at once. A worker thread catches each
//! transcript up from its stored [`Checkpoint`], then follows appends via
//! `notify` on `<root>/projects`, delivering parsed lines in bounded
//! [`Batch`]es over a bounded channel. Only the transcript layout from
//! [`crate::discovery`] is opened; `.credentials.json` and `sessions/`
//! never are.

mod reader;
mod store;
mod worker;

use std::collections::HashMap;
use std::io;
use std::panic::resume_unwind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Sender, bounded, unbounded};
use serde_json::Value;

use reader::Limits;
use worker::Worker;

pub use crossbeam_channel::Receiver;
pub use store::{Checkpoint, FileIdentity, FileOffsetStore, MemoryOffsetStore, OffsetStore};

/// One delivery: at most [`TailOptions::batch_records`] events, all from
/// the same file, in file order.
pub type Batch = Vec<TailEvent>;

#[derive(Debug, Clone, PartialEq)]
pub enum TailEvent {
    /// One parsed line. `offset` is the byte offset of the line's start.
    Record {
        path: Arc<Path>,
        offset: u64,
        value: Value,
    },
    /// A complete line that is not JSON; reading carried on past it.
    Malformed {
        path: Arc<Path>,
        offset: u64,
        error: String,
    },
    /// The file no longer matches its checkpoint, so it is re-read from 0.
    /// Records already delivered for this path are stale.
    Reset {
        path: Arc<Path>,
        reason: ResetReason,
    },
    /// The file could not be read, or its checkpoint could not be stored.
    Error { path: Arc<Path>, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetReason {
    /// Shorter than the stored offset.
    Truncated,
    /// A different file now sits at the path.
    Replaced,
}

#[derive(Debug, Clone)]
pub struct TailOptions {
    /// Most events in one [`Batch`].
    pub batch_records: usize,
    /// Batches buffered in the channel before the worker waits for the
    /// consumer.
    pub channel_batches: usize,
    /// Size of each file read.
    pub chunk_bytes: usize,
    /// How often transcript sizes are re-checked, as a backstop for
    /// filesystem events that `notify` drops or never gets.
    pub rescan_every: Duration,
}

impl Default for TailOptions {
    fn default() -> Self {
        Self {
            batch_records: 512,
            channel_batches: 8,
            chunk_bytes: 64 * 1024,
            rescan_every: Duration::from_secs(2),
        }
    }
}

/// A running tail. Dropping it stops the worker, as [`Tailer::stop`] does.
pub struct Tailer<S: OffsetStore + Send + 'static> {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<S>>,
    bytes_read: Arc<AtomicU64>,
}

impl<S: OffsetStore + Send + 'static> Tailer<S> {
    /// Starts tailing `root` (a Claude Code config dir) on a worker thread
    /// and returns at once with the receiving end of the batch channel.
    /// Fails only when the root path or the file watcher cannot be set up;
    /// a `projects/` dir that does not exist yet is picked up when it does.
    pub fn start(
        root: impl Into<PathBuf>,
        store: S,
        options: TailOptions,
    ) -> io::Result<(Self, Receiver<Batch>)> {
        // notify reports absolute paths; match them.
        let root = std::path::absolute(root.into())?;
        let (fs_tx, fs_events) = unbounded();
        let watcher = notify::recommended_watcher(move |event| {
            let _gone = fs_tx.send(event);
        })
        .map_err(io::Error::other)?;
        let (out, batches) = bounded(options.channel_batches);
        let (stop, stop_rx) = bounded(0);
        let bytes_read = Arc::new(AtomicU64::new(0));
        let worker = Worker {
            root,
            store,
            limits: Limits {
                chunk_bytes: options.chunk_bytes,
                batch_records: options.batch_records,
            },
            rescan_every: options.rescan_every,
            bytes_read: bytes_read.clone(),
            out,
            stop: stop_rx,
            watcher,
            fs_events,
            watching: false,
            seen: HashMap::new(),
        };
        let worker = thread::Builder::new()
            .name("strate-tail".into())
            .spawn(move || worker.run())?;
        let tailer = Self {
            stop: Some(stop),
            worker: Some(worker),
            bytes_read,
        };
        Ok((tailer, batches))
    }

    /// Transcript bytes read so far by this tailer.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// Stops the worker and hands back the store. Returns promptly even
    /// when the channel is full; batches already queued stay receivable.
    pub fn stop(mut self) -> S {
        self.stop.take();
        let worker = self.worker.take().expect("worker runs until stop");
        worker.join().unwrap_or_else(|panic| resume_unwind(panic))
    }
}

impl<S: OffsetStore + Send + 'static> Drop for Tailer<S> {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            let _store = worker.join();
        }
    }
}

/// A fresh, empty, uniquely named dir under the system temp dir.
#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("strate-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}
