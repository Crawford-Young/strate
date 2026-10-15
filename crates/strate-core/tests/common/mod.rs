//! Fixture loading shared by the tests that replay a synthetic home from
//! `tests/fixtures` through the store.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use strate_core::tail::TailEvent;

/// A temp dir removed on drop.
pub struct Temp(PathBuf);

impl Temp {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("strate-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    pub fn db(&self) -> PathBuf {
        self.0.join("strate.db")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The synthetic home `tests/fixtures/<name>`.
pub fn fixture_home(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Every transcript under `root/projects`, sorted.
pub fn transcripts(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("readable dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, out);
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
