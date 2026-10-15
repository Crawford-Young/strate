//! Fixture loading shared by the integration tests: temp dirs, the
//! synthetic homes under `tests/fixtures`, and their transcripts as record
//! events and batches.

#![allow(dead_code, reason = "each test binary uses a subset")]

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use strate_core::store::Store;
use strate_core::tail::{Batch, Checkpoint, FileIdentity, TailEvent};

/// A temp dir removed on drop.
pub struct Temp(PathBuf);

impl Temp {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn db(&self) -> PathBuf {
        self.0.join("strate.db")
    }

    /// The writer on [`Self::db`], and a reader on it.
    pub fn open(&self) -> (Store, Connection) {
        let db = self.db();
        let store = Store::open(&db).expect("store");
        let reader = Store::open_reader(&db).expect("reader");
        (store, reader)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `tests/fixtures`.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The synthetic home `tests/fixtures/<name>`.
pub fn fixture_home(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// Every transcript under `root/projects`, sorted. A `tool-results` dir
/// holds tool output, never a transcript.
pub fn transcripts(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("readable dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                if path.file_name() != Some("tool-results".as_ref()) {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join("projects"), &mut out);
    out.sort();
    out
}

/// Lines `range` of `path` as record events, each with the byte offset just
/// past its line.
pub fn records(path: &Path, range: Range<usize>) -> Vec<(TailEvent, u64)> {
    let text = fs::read_to_string(path).expect("read");
    let path: Arc<Path> = path.into();
    let mut offset = 0u64;
    let mut out = Vec::new();
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let end = offset + line.len() as u64;
        if range.contains(&n) {
            out.push((
                TailEvent::Record {
                    path: path.clone(),
                    offset,
                    value: serde_json::from_str(line).expect("fixture line is json"),
                },
                end,
            ));
        }
        offset = end;
    }
    out
}

/// The whole of `path` as one batch, each line a record at its byte offset,
/// checkpointed at the end of its last line.
pub fn batch(path: &Path) -> Batch {
    let records = records(path, 0..usize::MAX);
    let offset = records.last().map_or(0, |(_, end)| *end);
    Batch {
        path: path.into(),
        events: records.into_iter().map(|(event, _)| event).collect(),
        checkpoint: Some(Checkpoint {
            offset,
            identity: FileIdentity {
                volume: 0,
                index: 0,
            },
        }),
    }
}
