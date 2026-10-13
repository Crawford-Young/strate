//! Tauri commands over the store's read views. Each runs off the UI thread
//! (`async`) on one read-only connection; the engine thread keeps the only
//! writer. The webview refetches on [`STORE_CHANGED`] instead of polling.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;
use strate_core::store::Store;
use strate_core::store::views::{
    self, Audit, AuditFilter, LiveGraph, NeedsYou, NodeKind, TimelineDay, Workstream,
    WorkstreamCost,
};
use tauri::State;

use crate::engine::{Progress, Refresher, Status};

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

/// The Audit tree and headline for `filter`, over the stored cells and
/// the cached GitHub rows. A bad day in the filter is an error.
#[tauri::command(async)]
pub fn audit_rollup(reader: State<'_, Reader>, filter: AuditFilter) -> Result<Audit, String> {
    rollup(&reader, &filter)
}

fn rollup(reader: &Reader, filter: &AuditFilter) -> Result<Audit, String> {
    reader.read(|conn| views::audit(conn, filter))
}

/// The day timeline of the Audit node at `path` (its `(kind, key)` steps
/// from the root) under `filter`: spans in ms since each local midnight,
/// plus close and merge marks. An unknown path has no days; a bad day in
/// the filter is an error.
#[tauri::command(async)]
pub fn audit_timeline(
    reader: State<'_, Reader>,
    filter: AuditFilter,
    path: Vec<(NodeKind, String)>,
) -> Result<Vec<TimelineDay>, String> {
    timeline(&reader, &filter, &path)
}

fn timeline(
    reader: &Reader,
    filter: &AuditFilter,
    path: &[(NodeKind, String)],
) -> Result<Vec<TimelineDay>, String> {
    reader.read(|conn| views::audit_timeline(conn, filter, path))
}

/// Asks the engine to refresh the GitHub cache and returns at once; the
/// webview refetches on [`STORE_CHANGED`] when it lands. Requests during a
/// refresh coalesce into one more after it, and no GitHub failure is an
/// error here (it is stored per repo).
#[tauri::command]
pub fn audit_refresh(refresher: State<'_, Refresher>) {
    refresher.request();
}

#[cfg(test)]
mod tests {
    use strate_core::store::views::GroupBy;

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

        let filter = |from: &str| AuditFilter {
            from: from.into(),
            to: "2026-10-31".into(),
            repos: vec![],
            group_by: GroupBy::Hierarchy,
        };
        let audit = rollup(&reader, &filter("2026-10-01")).expect("an empty store rolls up");
        assert!(audit.root.children.is_empty());
        assert_eq!(audit.headline.issues_closed, 0);
        let err = rollup(&reader, &filter("October")).expect_err("a bad day is an error");
        assert!(!err.is_empty());
        let path = vec![(NodeKind::Repo, "demo".to_string())];
        assert_eq!(timeline(&reader, &filter("2026-10-01"), &path), Ok(vec![]));
        let err = timeline(&reader, &filter("October"), &path).expect_err("a bad day");
        assert!(!err.is_empty());

        assert!(Reader::open(&dir.join("missing.db")).is_err());
        drop(reader);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
