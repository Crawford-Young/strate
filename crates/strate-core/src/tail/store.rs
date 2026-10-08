use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use file_id::FileId;
use serde_json::{Map, Value, json};

/// Which file a stored offset belongs to: the volume plus the file's id on
/// it (device + inode on Unix, volume serial + file id on Windows). A path
/// whose identity changed was replaced, so its offset no longer applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    pub volume: u64,
    pub index: u128,
}

impl FileIdentity {
    /// Reads the identity of the file at `path` from file metadata; no file
    /// content is read.
    pub fn of(path: &Path) -> io::Result<Self> {
        Ok(match file_id::get_file_id(path)? {
            FileId::Inode {
                device_id,
                inode_number,
            } => Self {
                volume: device_id,
                index: inode_number.into(),
            },
            FileId::LowRes {
                volume_serial_number,
                file_index,
            } => Self {
                volume: volume_serial_number.into(),
                index: file_index.into(),
            },
            FileId::HighRes {
                volume_serial_number,
                file_id,
            } => Self {
                volume: volume_serial_number,
                index: file_id,
            },
        })
    }
}

/// How far one file has been ingested: every byte before `offset` ends in a
/// complete line that has been delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub offset: u64,
    pub identity: FileIdentity,
}

/// Persists a [`Checkpoint`] per transcript path, so a restart resumes
/// where the last run stopped.
pub trait OffsetStore {
    fn get(&self, path: &Path) -> Option<Checkpoint>;
    fn set(&mut self, path: &Path, checkpoint: Checkpoint) -> io::Result<()>;
}

/// An [`OffsetStore`] that lives as long as the process.
#[derive(Debug, Clone, Default)]
pub struct MemoryOffsetStore {
    offsets: HashMap<PathBuf, Checkpoint>,
}

impl OffsetStore for MemoryOffsetStore {
    fn get(&self, path: &Path) -> Option<Checkpoint> {
        self.offsets.get(path).copied()
    }

    fn set(&mut self, path: &Path, checkpoint: Checkpoint) -> io::Result<()> {
        self.offsets.insert(path.to_path_buf(), checkpoint);
        Ok(())
    }
}

/// An [`OffsetStore`] kept in one JSON file, rewritten (temp file + rename)
/// on every `set`. A stand-in until the SQLite store (#12).
#[derive(Debug)]
pub struct FileOffsetStore {
    file: PathBuf,
    offsets: MemoryOffsetStore,
}

impl FileOffsetStore {
    /// Loads `file` if it exists, else starts empty. A file that is not a
    /// store written by this type is `InvalidData`.
    pub fn open(file: impl Into<PathBuf>) -> io::Result<Self> {
        let file = file.into();
        let mut offsets = MemoryOffsetStore::default();
        match fs::read(&file) {
            Ok(bytes) => {
                let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not an offset store");
                let Ok(Value::Object(entries)) = serde_json::from_slice(&bytes) else {
                    return Err(invalid());
                };
                for (path, entry) in entries {
                    let checkpoint = decode(&entry).ok_or_else(invalid)?;
                    offsets.set(Path::new(&path), checkpoint)?;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(Self { file, offsets })
    }
}

/// `{"offset": n, "volume": n, "index": "n"}`; the u128 index is a string
/// because JSON numbers stop short of it.
fn encode(checkpoint: &Checkpoint) -> Value {
    json!({
        "offset": checkpoint.offset,
        "volume": checkpoint.identity.volume,
        "index": checkpoint.identity.index.to_string(),
    })
}

fn decode(entry: &Value) -> Option<Checkpoint> {
    Some(Checkpoint {
        offset: entry.get("offset")?.as_u64()?,
        identity: FileIdentity {
            volume: entry.get("volume")?.as_u64()?,
            index: entry.get("index")?.as_str()?.parse().ok()?,
        },
    })
}

impl OffsetStore for FileOffsetStore {
    fn get(&self, path: &Path) -> Option<Checkpoint> {
        self.offsets.get(path)
    }

    fn set(&mut self, path: &Path, checkpoint: Checkpoint) -> io::Result<()> {
        self.offsets.set(path, checkpoint)?;
        let entries: Map<String, Value> = self
            .offsets
            .offsets
            .iter()
            .map(|(p, c)| (p.to_string_lossy().into_owned(), encode(c)))
            .collect();
        let tmp = self.file.with_extension("tmp");
        fs::write(&tmp, Value::Object(entries).to_string())?;
        fs::rename(&tmp, &self.file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tail::test_dir;
    use std::fs;

    fn checkpoint(offset: u64) -> Checkpoint {
        Checkpoint {
            offset,
            identity: FileIdentity {
                volume: 7,
                index: u128::MAX - 1,
            },
        }
    }

    #[test]
    fn memory_store_returns_what_was_set() {
        let mut store = MemoryOffsetStore::default();
        assert_eq!(store.get(Path::new("/a.jsonl")), None);
        store
            .set(Path::new("/a.jsonl"), checkpoint(42))
            .expect("set");
        assert_eq!(store.get(Path::new("/a.jsonl")), Some(checkpoint(42)));
        assert_eq!(store.get(Path::new("/b.jsonl")), None);
    }

    #[test]
    fn file_store_survives_reopen() {
        let dir = test_dir("store-reopen");
        let file = dir.join("offsets.json");
        let mut store = FileOffsetStore::open(&file).expect("open empty");
        assert_eq!(store.get(Path::new("/a.jsonl")), None);
        store
            .set(Path::new("/a.jsonl"), checkpoint(1))
            .expect("set");
        store
            .set(Path::new("/a.jsonl"), checkpoint(99))
            .expect("set");
        store
            .set(Path::new("/b c.jsonl"), checkpoint(5))
            .expect("set");
        drop(store);

        let reopened = FileOffsetStore::open(&file).expect("reopen");
        assert_eq!(reopened.get(Path::new("/a.jsonl")), Some(checkpoint(99)));
        assert_eq!(reopened.get(Path::new("/b c.jsonl")), Some(checkpoint(5)));
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn file_store_rejects_a_foreign_file() {
        let dir = test_dir("store-foreign");
        for junk in ["not json", "[1,2]", "{\"/a\":{\"offset\":\"x\"}}"] {
            let file = dir.join("offsets.json");
            fs::write(&file, junk).expect("write");
            let err = FileOffsetStore::open(&file).expect_err(junk);
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{junk}");
        }
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn identity_is_stable_per_file_and_differs_between_files() {
        let dir = test_dir("identity");
        let (a, b) = (dir.join("a"), dir.join("b"));
        fs::write(&a, "x").expect("write");
        fs::write(&b, "x").expect("write");
        let id_a = FileIdentity::of(&a).expect("id a");
        fs::write(&a, "longer now").expect("rewrite in place");
        assert_eq!(FileIdentity::of(&a).expect("id a again"), id_a);
        assert_ne!(FileIdentity::of(&b).expect("id b"), id_a);
        assert!(FileIdentity::of(&dir.join("missing")).is_err());
        fs::remove_dir_all(dir).expect("cleanup");
    }
}
