use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Batch, Checkpoint, FileIdentity, Offsets, ResetReason, TailEvent};
use crate::discovery::parse_line;

/// The consumer is gone or the tailer is stopping.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Stopped;

pub(super) struct Limits {
    pub chunk_bytes: usize,
    pub batch_records: usize,
}

/// Reads `path` from its checkpoint in `offsets` to EOF in `chunk_bytes`
/// reads, handing complete lines to `send` in batches of at most
/// `batch_records` events. Each batch carries the checkpoint it ends at;
/// `offsets` advances to it only after `send` accepts the batch, and only
/// through the last complete line: a partial trailing line is left for the
/// next call. Nothing is persisted here: the consumer stores a batch's
/// events and checkpoint together. Returns the position read up to, or
/// `None` when the file is gone or unreadable.
pub(super) fn tail_file(
    path: &Arc<Path>,
    offsets: &mut Offsets,
    limits: &Limits,
    bytes_read: &AtomicU64,
    send: &mut dyn FnMut(Batch) -> Result<(), Stopped>,
) -> Result<Option<u64>, Stopped> {
    let error = |message: String| Batch {
        path: path.clone(),
        events: vec![TailEvent::Error {
            path: path.clone(),
            message,
        }],
        checkpoint: None,
    };
    let opened = File::open(path).and_then(|file| {
        let len = file.metadata()?.len();
        Ok((file, len, FileIdentity::of(path)?))
    });
    let (mut file, len, identity) = match opened {
        Ok(opened) => opened,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return send(error(e.to_string())).map(|()| None),
    };

    let mut tail = Tail {
        path,
        offsets,
        identity,
        events: Vec::new(),
        committed: 0,
        delivered: None,
    };
    let reset = match tail.offsets.get(&**path) {
        Some(c) if c.identity != identity => Some(ResetReason::Replaced),
        Some(c) if len < c.offset => Some(ResetReason::Truncated),
        Some(c) => {
            tail.committed = c.offset;
            tail.delivered = Some(c.offset);
            None
        }
        None => None,
    };
    if let Some(reason) = reset {
        tail.events.push(TailEvent::Reset {
            path: path.clone(),
            reason,
        });
    } else if len == tail.committed {
        return Ok(Some(len));
    }
    if let Err(e) = file.seek(SeekFrom::Start(tail.committed)) {
        tail.flush(send)?;
        return send(error(e.to_string())).map(|()| None);
    }

    let mut buf = vec![0; limits.chunk_bytes.max(1)];
    let mut carry = Vec::new();
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                tail.events.extend(error(e.to_string()).events);
                break;
            }
        };
        bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        let mut rest = &buf[..n];
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            let line: &[u8] = if carry.is_empty() {
                &rest[..=i]
            } else {
                carry.extend_from_slice(&rest[..=i]);
                &carry
            };
            tail.push_line(line);
            carry.clear();
            rest = &rest[i + 1..];
            if tail.events.len() >= limits.batch_records {
                tail.flush(send)?;
            }
        }
        carry.extend_from_slice(rest);
    }
    let held = carry.len() as u64;
    tail.flush(send)?;
    Ok(Some(tail.committed + held))
}

/// One file's pass: events not yet sent and the offset they end at.
struct Tail<'a> {
    path: &'a Arc<Path>,
    offsets: &'a mut Offsets,
    identity: FileIdentity,
    events: Vec<TailEvent>,
    /// End of the last complete line read.
    committed: u64,
    /// Offset the last accepted batch ended at.
    delivered: Option<u64>,
}

impl Tail<'_> {
    fn push_line(&mut self, line: &[u8]) {
        let offset = self.committed;
        self.committed += line.len() as u64;
        let path = self.path.clone();
        match parse_line(line) {
            None => {}
            Some(Ok(value)) => self.events.push(TailEvent::Record {
                path,
                offset,
                value,
            }),
            Some(Err(e)) => self.events.push(TailEvent::Malformed {
                path,
                offset,
                error: e.to_string(),
            }),
        }
    }

    /// Sends pending events with the checkpoint they end at, then records
    /// that checkpoint as delivered. A pass that moved the offset without
    /// producing events (blank lines) still sends its checkpoint.
    fn flush(&mut self, send: &mut dyn FnMut(Batch) -> Result<(), Stopped>) -> Result<(), Stopped> {
        if self.events.is_empty() && self.delivered == Some(self.committed) {
            return Ok(());
        }
        let checkpoint = Checkpoint {
            offset: self.committed,
            identity: self.identity,
        };
        send(Batch {
            path: self.path.clone(),
            events: std::mem::take(&mut self.events),
            checkpoint: Some(checkpoint),
        })?;
        self.offsets.insert(self.path.to_path_buf(), checkpoint);
        self.delivered = Some(self.committed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::tail::{ResetReason, TailEvent, test_dir};

    const LIMITS: Limits = Limits {
        chunk_bytes: 64 * 1024,
        batch_records: 512,
    };

    struct Harness {
        dir: PathBuf,
        path: Arc<Path>,
        offsets: Offsets,
        bytes_read: AtomicU64,
    }

    impl Harness {
        fn new(name: &str, content: &str) -> Self {
            let dir = test_dir(name);
            let path: Arc<Path> = dir.join("s.jsonl").into();
            fs::write(&path, content).expect("write");
            Self {
                dir,
                path,
                offsets: Offsets::new(),
                bytes_read: AtomicU64::new(0),
            }
        }

        fn append(&self, text: &str) {
            let mut f = OpenOptions::new()
                .append(true)
                .open(&self.path)
                .expect("open for append");
            f.write_all(text.as_bytes()).expect("append");
        }

        /// One `tail_file` pass: (batches, returned position, bytes read).
        fn pass(&mut self, limits: &Limits) -> (Vec<Batch>, Option<u64>, u64) {
            let before = self.bytes_read.load(Ordering::Relaxed);
            let mut batches = Vec::new();
            let pos = tail_file(
                &self.path,
                &mut self.offsets,
                limits,
                &self.bytes_read,
                &mut |b| {
                    batches.push(b);
                    Ok(())
                },
            )
            .expect("never stopped");
            let read = self.bytes_read.load(Ordering::Relaxed) - before;
            (batches, pos, read)
        }

        fn offset(&self) -> Option<u64> {
            self.offsets.get(&*self.path).map(|c| c.offset)
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// (offset, value["n"]) of every record, in order.
    fn records(batches: &[Batch]) -> Vec<(u64, i64)> {
        batches
            .iter()
            .flat_map(|b| &b.events)
            .filter_map(|e| match e {
                TailEvent::Record { offset, value, .. } => {
                    Some((*offset, value["n"].as_i64().expect("n")))
                }
                _ => None,
            })
            .collect()
    }

    fn checkpoint_offsets(batches: &[Batch]) -> Vec<Option<u64>> {
        batches
            .iter()
            .map(|b| b.checkpoint.map(|c| c.offset))
            .collect()
    }

    #[test]
    fn emits_each_complete_line_with_its_byte_offset() {
        // Offsets by hand: line one is 8 bytes, the blank line 1, so the
        // third line starts at 9.
        let mut h = Harness::new("reader-basic", "{\"n\":1}\n\n{\"n\":2}\n");
        let (batches, pos, read) = h.pass(&LIMITS);
        assert_eq!(records(&batches), vec![(0, 1), (9, 2)]);
        assert_eq!(pos, Some(17));
        assert_eq!(read, 17);
        assert_eq!(h.offset(), Some(17));
        assert_eq!(batches[0].path, h.path);
        let TailEvent::Record { path, .. } = &batches[0].events[0] else {
            panic!("first event is a record: {batches:?}");
        };
        assert_eq!(path, &h.path);
    }

    #[test]
    fn each_batch_carries_the_checkpoint_it_ends_at() {
        let lines: String = (0..10).map(|n| format!("{{\"n\":{n}}}\n")).collect();
        let mut h = Harness::new("reader-batch-checkpoint", &lines);
        let limits = Limits {
            chunk_bytes: 64,
            batch_records: 3,
        };
        let (batches, _, _) = h.pass(&limits);
        // Eight bytes per line; batches of 3, 3, 3, 1 lines.
        assert_eq!(
            checkpoint_offsets(&batches),
            vec![Some(24), Some(48), Some(72), Some(80)]
        );
        let identity = FileIdentity::of(&h.path).expect("identity");
        assert!(
            batches
                .iter()
                .all(|b| b.checkpoint.map(|c| c.identity) == Some(identity)),
            "{batches:?}"
        );
    }

    #[test]
    fn blank_lines_alone_still_deliver_their_checkpoint() {
        let mut h = Harness::new("reader-blank", "{\"n\":1}\n");
        h.pass(&LIMITS);
        h.append("\n\n");
        let (batches, _, _) = h.pass(&LIMITS);
        assert_eq!(batches.len(), 1, "{batches:?}");
        assert!(batches[0].events.is_empty());
        assert_eq!(checkpoint_offsets(&batches), vec![Some(10)]);
        assert_eq!(h.offset(), Some(10));
    }

    #[test]
    fn partial_trailing_line_is_held_until_its_newline_arrives() {
        let mut h = Harness::new("reader-partial", "{\"n\":1}\n{\"n\":2,\"x\":\"ab");
        let (batches, pos, _) = h.pass(&LIMITS);
        assert_eq!(records(&batches), vec![(0, 1)]);
        assert_eq!(h.offset(), Some(8), "commit stops at the last newline");
        assert_eq!(checkpoint_offsets(&batches), vec![Some(8)]);
        assert_eq!(pos, Some(22), "the 14 held bytes were read");
        assert!(
            batches
                .iter()
                .flat_map(|b| &b.events)
                .all(|e| matches!(e, TailEvent::Record { .. })),
            "a partial line is not malformed: {batches:?}"
        );

        h.append("c\"}\n");
        let (batches, _, read) = h.pass(&LIMITS);
        assert_eq!(records(&batches), vec![(8, 2)]);
        assert_eq!(read, 18, "re-reads only the uncommitted tail");
        assert_eq!(h.offset(), Some(26));
    }

    #[test]
    fn resuming_reads_only_bytes_after_the_checkpoint() {
        let mut h = Harness::new("reader-resume", "{\"n\":1}\n{\"n\":2}\n");
        h.pass(&LIMITS);
        let (batches, _, read) = h.pass(&LIMITS);
        assert!(batches.is_empty(), "{batches:?}");
        assert_eq!(read, 0, "nothing new, nothing read");

        h.append("{\"n\":3}\n");
        let (batches, _, read) = h.pass(&LIMITS);
        assert_eq!(records(&batches), vec![(16, 3)]);
        assert_eq!(read, 8);
    }

    #[test]
    fn truncated_file_resets_to_zero_and_says_so() {
        let mut h = Harness::new("reader-truncate", "{\"n\":1}\n{\"n\":2}\n");
        h.pass(&LIMITS);
        fs::write(&h.path, "{\"n\":9}\n").expect("truncate in place");
        let (batches, _, _) = h.pass(&LIMITS);
        assert_eq!(
            batches[0].events[0],
            TailEvent::Reset {
                path: h.path.clone(),
                reason: ResetReason::Truncated,
            }
        );
        assert_eq!(records(&batches), vec![(0, 9)]);
        assert_eq!(h.offset(), Some(8));
    }

    #[test]
    fn replaced_file_resets_to_zero_and_says_so() {
        let mut h = Harness::new("reader-replace", "{\"n\":1}\n");
        h.pass(&LIMITS);
        // Longer than before, so only the identity check can catch it.
        let tmp = h.dir.join("s.jsonl.tmp");
        fs::write(&tmp, "{\"n\":7}\n{\"n\":8}\n").expect("write replacement");
        fs::rename(&tmp, &h.path).expect("replace");
        let (batches, _, _) = h.pass(&LIMITS);
        assert_eq!(
            batches[0].events[0],
            TailEvent::Reset {
                path: h.path.clone(),
                reason: ResetReason::Replaced,
            }
        );
        assert_eq!(records(&batches), vec![(0, 7), (8, 8)]);
    }

    #[test]
    fn replaced_empty_file_still_reports_the_reset() {
        let mut h = Harness::new("reader-replace-empty", "{\"n\":1}\n");
        h.pass(&LIMITS);
        let tmp = h.dir.join("s.jsonl.tmp");
        fs::write(&tmp, "").expect("write replacement");
        fs::rename(&tmp, &h.path).expect("replace");
        let (batches, _, _) = h.pass(&LIMITS);
        assert_eq!(batches.len(), 1, "{batches:?}");
        assert!(matches!(batches[0].events[..], [TailEvent::Reset { .. }]));
        assert_eq!(checkpoint_offsets(&batches), vec![Some(0)]);
        assert_eq!(h.offset(), Some(0));
    }

    #[test]
    fn malformed_line_is_reported_with_its_offset_and_reading_continues() {
        let mut h = Harness::new("reader-malformed", "{\"n\":1}\n{\"n\": \n{\"n\":3}\n");
        let (batches, _, _) = h.pass(&LIMITS);
        assert_eq!(records(&batches), vec![(0, 1), (15, 3)]);
        let malformed: Vec<u64> = batches
            .iter()
            .flat_map(|b| &b.events)
            .filter_map(|e| match e {
                TailEvent::Malformed { offset, .. } => Some(*offset),
                _ => None,
            })
            .collect();
        assert_eq!(malformed, vec![8]);
        assert_eq!(h.offset(), Some(23));
    }

    #[test]
    fn batches_are_bounded_and_lines_longer_than_a_read_survive() {
        let lines: String = (0..10).map(|n| format!("{{\"n\":{n}}}\n")).collect();
        let mut h = Harness::new("reader-batches", &lines);
        let tiny = Limits {
            chunk_bytes: 3,
            batch_records: 3,
        };
        let (batches, _, read) = h.pass(&tiny);
        let sizes: Vec<usize> = batches.iter().map(|b| b.events.len()).collect();
        assert_eq!(sizes, vec![3, 3, 3, 1]);
        let expected: Vec<(u64, i64)> = (0..10).map(|n| (n * 8, n as i64)).collect();
        assert_eq!(records(&batches), expected);
        assert_eq!(read, 80);
        assert_eq!(h.offset(), Some(80));
    }

    #[test]
    fn checkpoint_only_advances_after_a_batch_is_accepted() {
        let lines: String = (0..4).map(|n| format!("{{\"n\":{n}}}\n")).collect();
        let mut h = Harness::new("reader-stopped", &lines);
        let limits = Limits {
            chunk_bytes: 64,
            batch_records: 2,
        };
        let mut accepted = 0;
        let got = tail_file(&h.path, &mut h.offsets, &limits, &h.bytes_read, &mut |_| {
            accepted += 1;
            if accepted == 1 { Ok(()) } else { Err(Stopped) }
        });
        assert_eq!(got, Err(Stopped));
        assert_eq!(h.offset(), Some(16), "first batch delivered, second not");
    }

    #[test]
    fn missing_file_yields_nothing() {
        let mut h = Harness::new("reader-missing", "");
        fs::remove_file(&h.path).expect("remove");
        let (batches, pos, read) = h.pass(&LIMITS);
        assert!(batches.is_empty());
        assert_eq!((pos, read), (None, 0));
        assert_eq!(h.offset(), None);
    }
}
