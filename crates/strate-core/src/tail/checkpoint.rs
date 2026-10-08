use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use file_id::FileId;

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

/// A [`Checkpoint`] per transcript path. The tailer starts from the store's
/// committed offsets and then tracks how far it has delivered in memory.
pub type Offsets = HashMap<PathBuf, Checkpoint>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tail::test_dir;
    use std::fs;

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
