use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, select, tick};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use super::reader::{Limits, Stopped, tail_file};
use super::{Batch, Offsets, TailEvent};
use crate::discovery::{is_transcript, parse_line, transcript_paths};
use crate::time::parse_timestamp;

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
        let paths = read_order(transcript_paths(&self.root), &self.offsets);
        self.scan(paths)?;
        self.send(Batch {
            path: self.root.as_path().into(),
            events: vec![TailEvent::CaughtUp],
            checkpoint: None,
        })?;
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
    /// last pass. Directory listings and metadata only: an unchanged file is
    /// not opened.
    fn rescan(&mut self) -> Result<(), Stopped> {
        self.scan(transcript_paths(&self.root))
    }

    /// Tails each of `paths`, in order, whose size moved since the last pass
    /// (all of them on the first).
    fn scan(&mut self, paths: Vec<PathBuf>) -> Result<(), Stopped> {
        if !self.watching {
            self.watching = self
                .watcher
                .watch(&self.root.join("projects"), RecursiveMode::Recursive)
                .is_ok();
        }
        for path in paths {
            let len = fs::metadata(&path).map(|m| m.len()).ok();
            if len.is_some() && self.seen.get(&path) != len.as_ref() {
                self.tail(&path)?;
            }
        }
        Ok(())
    }

    fn send(&self, batch: Batch) -> Result<(), Stopped> {
        select! {
            send(self.out, batch) -> sent => sent.map_err(|_| Stopped),
            recv(self.stop) -> _ => Err(Stopped),
        }
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

/// The order of the first scan, which keeps a replayed uuid (a resume
/// re-recording its original) with the original whatever the file names
/// sort to, as claude-config's audit-lib does (3ee4b2e): the store keeps
/// the first copy it reads. Files already part-read come first, as listed:
/// their earlier records are stored. Unread main transcripts follow, oldest
/// first by the (earliest, latest) stamp they hold, then path; one with no
/// stamp goes last. A resume replays its original's records with their
/// stamps and appends later ones, so it sorts after the original; an
/// original that kept growing past its fork sorts after the fork, which then
/// keeps the replays (audit-lib's tie). Unread subagents go last, as listed.
fn read_order(paths: Vec<PathBuf>, offsets: &Offsets) -> Vec<PathBuf> {
    let (started, unread): (Vec<_>, Vec<_>) =
        paths.into_iter().partition(|p| offsets.contains_key(p));
    let (subagents, mains): (Vec<_>, Vec<_>) = unread
        .into_iter()
        .partition(|p| p.parent().and_then(Path::file_name) == Some("subagents".as_ref()));
    let mut mains: Vec<_> = mains.into_iter().map(|p| (span(&p), p)).collect();
    mains.sort_by(|(a, x), (b, y)| (a.is_none(), a, x).cmp(&(b.is_none(), b, y)));
    started
        .into_iter()
        .chain(mains.into_iter().map(|(_, p)| p))
        .chain(subagents)
        .collect()
}

/// The earliest and latest record stamp in `path`, in epoch ms; `None` when
/// it holds none or can't be read.
fn span(path: &Path) -> Option<(i64, i64)> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut line = Vec::new();
    let mut span: Option<(i64, i64)> = None;
    while reader.read_until(b'\n', &mut line).ok()? > 0 {
        let stamp = parse_line(&line)
            .and_then(Result::ok)
            .and_then(|v| v.get("timestamp")?.as_str().and_then(parse_timestamp));
        if let Some(t) = stamp {
            span = Some(span.map_or((t, t), |(lo, hi)| (lo.min(t), hi.max(t))));
        }
        line.clear();
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tail::{Checkpoint, FileIdentity, test_dir};

    /// Writes a transcript of one record per stamp (`None`: no stamp)
    /// under `root/projects/-work-x`; returns its path.
    fn transcript(root: &Path, rel: &str, stamps: &[Option<&str>]) -> PathBuf {
        let path = root.join("projects/-work-x").join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let lines: String = stamps
            .iter()
            .map(|stamp| match stamp {
                Some(t) => format!("{{\"type\":\"user\",\"timestamp\":\"{t}\"}}\n"),
                None => "{\"type\":\"user\"}\n".to_string(),
            })
            .collect();
        fs::write(&path, lines).expect("write");
        path
    }

    const T0: &str = "2026-01-01T00:00:00.000Z";
    const T1: &str = "2026-01-01T00:01:00.000Z";
    const T2: &str = "2026-01-01T00:02:00.000Z";
    const T5: &str = "2026-01-01T00:05:00.000Z";

    #[test]
    fn the_first_scan_reads_mains_oldest_first_then_subagents() {
        let root = test_dir("read-order");
        let resume = transcript(&root, "a.jsonl", &[Some(T0), Some(T1), Some(T5)]);
        let original = transcript(&root, "b.jsonl", &[Some(T1), Some(T0)]);
        let unstamped = transcript(&root, "c.jsonl", &[None]);
        let twin = transcript(&root, "d.jsonl", &[Some(T0), Some(T1)]);
        let sub = transcript(&root, "a/subagents/agent-x.jsonl", &[Some("bad"), None]);
        let started = transcript(&root, "e.jsonl", &[Some(T5)]);
        let mut offsets = Offsets::new();
        let identity = FileIdentity {
            volume: 0,
            index: 0,
        };
        offsets.insert(
            started.clone(),
            Checkpoint {
                offset: 1,
                identity,
            },
        );
        let missing = root.join("projects/-work-x/gone.jsonl");
        let paths = vec![
            resume.clone(),
            sub.clone(),
            original.clone(),
            unstamped.clone(),
            twin.clone(),
            started.clone(),
            missing.clone(),
        ];
        assert_eq!(
            read_order(paths, &offsets),
            // A file already part-read resumes first; equal spans fall back
            // to the path; a file with no stamp (or none readable) goes last
            // among the mains.
            vec![started, original, twin, resume, unstamped, missing, sub]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_original_that_outgrows_its_fork_is_read_after_it() {
        // audit-lib's tie (claude-config 3ee4b2e): spans can't tell a fork
        // from an original that kept going, so the fork keeps the replays.
        let root = test_dir("read-order-fork");
        let original = transcript(&root, "o.jsonl", &[Some(T0), Some(T1), Some(T5)]);
        let fork = transcript(&root, "f.jsonl", &[Some(T0), Some(T1), Some(T2)]);
        assert_eq!(
            read_order(vec![original.clone(), fork.clone()], &Offsets::new()),
            vec![fork, original]
        );
        let _ = fs::remove_dir_all(&root);
    }
}
