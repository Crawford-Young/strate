use rusqlite::Connection;

/// Append-only: never edit a shipped entry, add the next one. Entry `i`
/// takes a db from `user_version` `i` to `i + 1`.
pub(super) const MIGRATIONS: &[&str] = &[
    include_str!("0001_init.sql"),
    include_str!("0002_workstreams.sql"),
    include_str!("0003_cost_time.sql"),
    include_str!("0004_live.sql"),
    include_str!("0005_github.sql"),
    include_str!("0006_audit.sql"),
    include_str!("0007_audit_review.sql"),
    include_str!("0008_performance.sql"),
    include_str!("0009_directs.sql"),
    include_str!("0010_audit_indexes.sql"),
    include_str!("0011_permission_waits.sql"),
    include_str!("0012_timeline_index.sql"),
    include_str!("0013_latest_links.sql"),
    include_str!("0014_drop_registry_copies.sql"),
    include_str!("0015_workstream_costs.sql"),
    include_str!("0016_teammate_parents.sql"),
    include_str!("0017_idle_notices.sql"),
    include_str!("0018_registry_wait_start.sql"),
    include_str!("0019_teammate_types.sql"),
    include_str!("0020_drop_gear.sql"),
    include_str!("0021_agent_model.sql"),
    include_str!("0022_drop_workstreams.sql"),
];

/// Applies every migration past the db's `user_version`, each in its own
/// transaction together with its version bump. A db already at the latest
/// version is left untouched; one newer than `migrations` is refused.
pub(super) fn migrate(conn: &mut Connection, migrations: &[&str]) -> rusqlite::Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let current = usize::try_from(current).unwrap_or(usize::MAX);
    if current > migrations.len() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "store schema v{current} is newer than this build (v{})",
                migrations.len()
            )),
        ));
    }
    for (done, sql) in migrations.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", done as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version")
    }

    const M1: &str = "CREATE TABLE t1 (x INTEGER); INSERT INTO t1 VALUES (1);";
    // No IF NOT EXISTS: running it twice would fail.
    const M2: &str = "CREATE TABLE t2 (y INTEGER);";

    #[test]
    fn an_existing_db_is_upgraded_and_a_reopen_is_a_no_op() {
        let mut conn = Connection::open_in_memory().expect("db");
        migrate(&mut conn, &[M1]).expect("v1");
        assert_eq!(version(&conn), 1);
        conn.execute("INSERT INTO t1 VALUES (2)", []).expect("row");

        migrate(&mut conn, &[M1, M2]).expect("v1 -> v2");
        assert_eq!(version(&conn), 2);
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM t1", [], |r| r.get(0))
            .expect("t1");
        assert_eq!(rows, 2, "v1 data kept, M1 not re-run");
        conn.execute("INSERT INTO t2 VALUES (1)", [])
            .expect("t2 exists");

        migrate(&mut conn, &[M1, M2]).expect("already current");
        assert_eq!(version(&conn), 2);
    }

    #[test]
    fn a_failing_migration_rolls_back_whole_and_keeps_the_version() {
        let mut conn = Connection::open_in_memory().expect("db");
        migrate(&mut conn, &[M1]).expect("v1");
        let broken = "CREATE TABLE t3 (z INTEGER); SELECT * FROM no_such_table;";
        assert!(migrate(&mut conn, &[M1, broken]).is_err());
        assert_eq!(version(&conn), 1);
        let t3: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 't3'",
                [],
                |r| r.get(0),
            )
            .expect("schema");
        assert_eq!(t3, 0, "the half-applied migration rolled back");
    }

    #[test]
    fn a_db_newer_than_this_build_is_refused() {
        let mut conn = Connection::open_in_memory().expect("db");
        migrate(&mut conn, &[M1, M2]).expect("v2");
        let err = migrate(&mut conn, &[M1]).expect_err("v2 db, v1 build");
        assert!(err.to_string().contains("newer"), "{err}");
        assert_eq!(version(&conn), 2);
    }

    #[test]
    fn v16_hangs_a_leadless_teammate_under_its_sessions_orchestrator() {
        let mut conn = Connection::open_in_memory().expect("db");
        migrate(&mut conn, &MIGRATIONS[..15]).expect("v15");
        // Teammates 3 and 4 have no stored lead; 5's lead is s1's
        // orchestrator; 6 is a subagent with no edge.
        conn.execute_batch(
            "INSERT INTO sessions (session_id) VALUES ('s1'), ('s2');
             INSERT INTO agents (id, session_id, agent_id, kind) VALUES
                 (1, 's1', NULL, 'orchestrator'), (2, 's2', NULL, 'orchestrator'),
                 (3, 's2', 'm1', 'teammate'), (4, 's1', 'm2', 'teammate'),
                 (5, 's2', 'm3', 'teammate'), (6, 's2', 'w1', 'subagent');
             INSERT INTO edges (kind, parent, child, team_name, team_dir) VALUES
                 ('teammate', NULL, 3, 'session-x', 'session-x'),
                 ('teammate', NULL, 4, 'session-y', NULL),
                 ('teammate', 1, 5, 'session-z', 'session-z');",
        )
        .expect("v15 rows");
        migrate(&mut conn, MIGRATIONS).expect("v16");
        let parents: Vec<(i64, Option<i64>)> = conn
            .prepare("SELECT child, parent FROM edges ORDER BY child")
            .expect("edges")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("rows")
            .collect::<rusqlite::Result<_>>()
            .expect("parents");
        assert_eq!(parents, [(3, Some(2)), (4, Some(1)), (5, Some(1))]);
    }

    #[test]
    fn the_shipped_migrations_apply_to_an_empty_db() {
        let mut conn = Connection::open_in_memory().expect("db");
        migrate(&mut conn, MIGRATIONS).expect("migrate");
        assert_eq!(version(&conn), MIGRATIONS.len() as i64);
    }
}
