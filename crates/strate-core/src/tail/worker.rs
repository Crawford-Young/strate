use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, select, tick};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use super::reader::{Limits, Stopped, tail_file};
use super::{Batch, Offsets};
use crate::discovery::{is_transcript, transcript_paths};

pub(super) struct Worker {
    pub root: PathBuf,
    /// Per transcript, the checkpoint of the last batch handed to the channel.
    pub offsets: Offsets,
    pub limits: Limits,
    pub rescan_every: Duration,
    pub bytes_read: Arc<AtomicU64>,
    pub out: Sender<Batch>,
    pub stop: Receiver<()>,
    pub watcher: RecommendedWatcher,
    pub fs_events: Receiver<notify::Result<Event>>,
    /// Whether `<root>/projects` is watched yet; retried on every rescan
    /// until it exists.
    pub watching: bool,
    /// Per transcript, the position the last pass read up to.
    pub seen: HashMap<PathBuf, u64>,
}

impl Worker {
    /// Runs until stopped or the consumer hangs up.
    pub fn run(mut self) {
        let _stopped = self.serve();
    }

    fn serve(&mut self) -> Result<(), Stopped> {
        self.rescan()?;
        let ticker = tick(self.rescan_every);
        loop {
            select! {
                recv(self.stop) -> _ => return Err(Stopped),
                recv(self.fs_events) -> first => {
                    // Coalesce everything already queued into one set.
                    let events: Vec<_> = first.into_iter().chain(self.fs_events.try_iter()).collect();
                    let mut dirty = BTreeSet::new();
                    let mut lost = false;
                    for event in events {
                        match event {
                            Ok(event) => {
                                lost |= event.need_rescan();
                                dirty.extend(event.paths.into_iter().filter(|p| is_transcript(&self.root, p)));
                            }
                            Err(_) => lost = true,
                        }
                    }
                    for path in dirty {
                        self.tail(&path)?;
                    }
                    if lost {
                        self.rescan()?;
                    }
                }
                recv(ticker) -> _ => self.rescan()?,
            }
        }
    }

    /// Re-lists every transcript and tails those whose size moved since the
    /// last pass (all of them on the first call). Directory listings and
    /// metadata only: an unchanged file is not opened.
    fn rescan(&mut self) -> Result<(), Stopped> {
        if !self.watching {
            self.watching = self
                .watcher
                .watch(&self.root.join("projects"), RecursiveMode::Recursive)
                .is_ok();
        }
        for path in transcript_paths(&self.root) {
            let len = fs::metadata(&path).map(|m| m.len()).ok();
            if len.is_some() && self.seen.get(&path) != len.as_ref() {
                self.tail(&path)?;
            }
        }
        Ok(())
    }

    fn tail(&mut self, path: &Path) -> Result<(), Stopped> {
        let shared: Arc<Path> = path.into();
        let (out, stop) = (&self.out, &self.stop);
        let read_to = tail_file(
            &shared,
            &mut self.offsets,
            &self.limits,
            &self.bytes_read,
            &mut |batch| {
                select! {
                    send(out, batch) -> sent => sent.map_err(|_| Stopped),
                    recv(stop) -> _ => Err(Stopped),
                }
            },
        )?;
        match read_to {
            Some(pos) => self.seen.insert(path.to_path_buf(), pos),
            None => self.seen.remove(path),
        };
        Ok(())
    }
}
