//! Tauri commands over the store's read views. Each runs off the UI thread
//! (`async`) on one read-only connection; the engine thread keeps the only
//! writer. The webview refetches on [`STORE_CHANGED`] instead of polling.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;
use strate_core::store::Store;
use strate_core::store::views::{self, LiveGraph, NeedsYou, Workstream, WorkstreamCost};
use tauri::State;

use crate::engine::{Progress, Status};

/// Emitted (no payload, at most four a second) after the store changed.
pub const STORE_CHANGED: &str = "store-changed";

/// The commands' read-only connection.
pub struct Reader(Mutex<Connection>);

impl Reader {
    pub fn open(db: &Path) -> rusqlite::Result<Self> {
        Store::open_reader(db).map(|conn| Self(Mutex::new(conn)))
    }

    /// Runs `view` on the connection; a SQLite error comes back as its
    /// message.
    pub fn read<T>(
        &self,
        view: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, String> {
        let conn = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        view(&conn).map_err(|e| e.to_string())
    }
}

/// `indexing` until the initial scan is stored, then `live`.
#[tauri::command(async)]
pub fn status(progress: State<'_, Arc<Progress>>) -> Status {
    progress.status()
}

/// Every workstream with its $, state, needs-you count and drawn session.
#[tauri::command(async)]
pub fn workstreams(reader: State<'_, Reader>) -> Result<Vec<Workstream>, String> {
    reader.read(views::workstreams)
}

#[tauri::command(async)]
pub fn workstream_costs(reader: State<'_, Reader>) -> Result<Vec<WorkstreamCost>, String> {
    reader.read(views::workstream_costs)
}

#[tauri::command(async)]
pub fn needs_you(reader: State<'_, Reader>) -> Result<Vec<NeedsYou>, String> {
    reader.read(views::needs_you)
}

/// The live session of `workstream`: its agents and dispatch and teammate
/// edges, never earlier sessions.
#[tauri::command(async)]
pub fn live_graph(reader: State<'_, Reader>, workstream: i64) -> Result<Option<LiveGraph>, String> {
    reader.read(|conn| views::live_graph(conn, workstream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_run_the_view_and_report_sqlite_errors_as_text() {
        let dir = std::env::temp_dir().join(format!("strate-commands-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("store"));

        let reader = Reader::open(&db).expect("reader");
        assert_eq!(reader.read(views::workstreams), Ok(vec![]));
        assert_eq!(reader.read(|conn| views::live_graph(conn, 1)), Ok(None));
        let err = reader
            .read(|conn| conn.execute("DELETE FROM events", []))
            .expect_err("read-only");
        assert!(err.contains("readonly"), "{err}");

        assert!(Reader::open(&dir.join("missing.db")).is_err());
        drop(reader);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
