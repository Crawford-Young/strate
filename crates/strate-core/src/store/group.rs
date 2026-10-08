//! Workstream grouping (#13). A pure function of the stored events, so
//! regrouping is idempotent and an incremental build converges to a cold
//! one. Every function runs inside the caller's transaction.
//!
//! A session transcript splits into name segments at each change of
//! `custom-title`; records before the first title take it (a late rename
//! backfills), and a non-final segment with no reply (the name `/clear`
//! carries into a new file) merges forward. A segment joins the workstream
//! of its name; a transcript with no title at all joins a fallback keyed by
//! its dominant cwd and branch.

use rusqlite::{Connection, params};

use super::Result;

/// The fields of one orchestrator record that grouping reads.
struct Row {
    offset: i64,
    reply: bool,
    timestamp: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    title: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Segment {
    start: i64,
    title: Option<String>,
    started_at: Option<String>,
}

fn earliest(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Splits rows (in transcript order) into name segments. The first segment
/// starts at 0, so every record of the transcript falls in one.
fn split(rows: &[Row]) -> Vec<Segment> {
    let mut raw: Vec<(Segment, bool)> = vec![(
        Segment {
            start: 0,
            title: None,
            started_at: None,
        },
        false,
    )];
    for row in rows {
        if let Some(title) = &row.title {
            let (current, _) = raw.last_mut().expect("never empty");
            match &current.title {
                None => current.title = Some(title.clone()),
                Some(name) if name == title => {}
                Some(_) => raw.push((
                    Segment {
                        start: row.offset,
                        title: Some(title.clone()),
                        started_at: None,
                    },
                    false,
                )),
            }
        }
        let (current, replied) = raw.last_mut().expect("never empty");
        *replied |= row.reply;
        current.started_at = earliest(current.started_at.take(), row.timestamp.clone());
    }
    let last = raw.len() - 1;
    let mut out = Vec::new();
    let mut prelude: Option<Segment> = None;
    for (i, (mut segment, replied)) in raw.into_iter().enumerate() {
        if let Some(p) = prelude.take() {
            segment.start = p.start;
            segment.started_at = earliest(p.started_at, segment.started_at);
        }
        if replied || i == last {
            out.push(segment);
        } else {
            prelude = Some(segment);
        }
    }
    out
}

/// The most frequent value, the earliest seen winning ties.
fn dominant<'a>(values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut tally: Vec<(&str, usize)> = Vec::new();
    for value in values {
        match tally.iter_mut().find(|(v, _)| *v == value) {
            Some((_, n)) => *n += 1,
            None => tally.push((value, 1)),
        }
    }
    tally.iter().rev().max_by_key(|(_, n)| *n).map(|(v, _)| *v)
}

fn fallback_name(rows: &[Row], session_id: &str) -> String {
    let cwd = dominant(rows.iter().filter_map(|r| r.cwd.as_deref()));
    let branch = dominant(rows.iter().filter_map(|r| r.branch.as_deref()));
    match (cwd, branch) {
        (Some(cwd), Some(branch)) => format!("{cwd} @ {branch}"),
        (Some(cwd), None) => cwd.to_string(),
        (None, _) => session_id.to_string(),
    }
}

fn workstream(conn: &Connection, name: &str, fallback: bool) -> Result<i64> {
    conn.prepare_cached(
        "INSERT INTO workstreams (name, fallback) VALUES (?1, ?2)
         ON CONFLICT (name, fallback) DO UPDATE SET name = excluded.name
         RETURNING id",
    )?
    .query_row(params![name, fallback], |r| r.get(0))
}

/// Regroups the given sessions, then drops workstreams left empty.
pub(super) fn sessions<'a>(
    conn: &Connection,
    ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    for id in ids {
        session(conn, id)?;
    }
    conn.prepare_cached(
        "DELETE FROM workstreams WHERE merged_into IS NULL
             AND NOT EXISTS (SELECT 1 FROM segments WHERE workstream_id = workstreams.id)
             AND NOT EXISTS (SELECT 1 FROM workstreams AS m WHERE m.merged_into = workstreams.id)",
    )?
    .execute([])?;
    Ok(())
}

/// Regroups every stored session.
pub(super) fn all(conn: &Connection) -> Result<()> {
    let ids: Vec<String> = conn
        .prepare("SELECT session_id FROM sessions ORDER BY session_id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    sessions(conn, ids.iter().map(String::as_str))
}

/// Rebuilds one session's segments, its workstream, its subagents'
/// segments and its process root's continuation edges.
fn session(conn: &Connection, session_id: &str) -> Result<()> {
    let rows: Vec<Row> = conn
        .prepare_cached(
            "SELECT byte_offset, type = 'assistant', timestamp, cwd, git_branch, custom_title
             FROM events WHERE session_id = ?1 AND agent_id IS NULL ORDER BY byte_offset",
        )?
        .query_map([session_id], |r| {
            Ok(Row {
                offset: r.get(0)?,
                reply: r.get::<_, Option<bool>>(1)?.unwrap_or(false),
                timestamp: r.get(2)?,
                cwd: r.get(3)?,
                branch: r.get(4)?,
                title: r.get(5)?,
            })
        })?
        .collect::<Result<_>>()?;
    let segments = if rows.is_empty() {
        Vec::new()
    } else {
        split(&rows)
    };

    let mut kept = Vec::with_capacity(segments.len());
    let mut current = None;
    for segment in &segments {
        let workstream = match &segment.title {
            Some(name) => workstream(conn, name, false)?,
            None => workstream(conn, &fallback_name(&rows, session_id), true)?,
        };
        let id: i64 = conn
            .prepare_cached(
                "INSERT INTO segments (session_id, start_offset, started_at, workstream_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (session_id, start_offset) DO UPDATE SET
                     started_at = excluded.started_at, workstream_id = excluded.workstream_id
                 RETURNING id",
            )?
            .query_row(
                params![session_id, segment.start, segment.started_at, workstream],
                |r| r.get(0),
            )?;
        kept.push(id);
        current = Some(workstream);
    }
    let stale: Vec<i64> = conn
        .prepare_cached("SELECT id FROM segments WHERE session_id = ?1")?
        .query_map([session_id], |r| r.get(0))?
        .collect::<Result<Vec<i64>>>()?
        .into_iter()
        .filter(|id| !kept.contains(id))
        .collect();
    for id in stale {
        conn.prepare_cached("DELETE FROM segments WHERE id = ?1")?
            .execute([id])?;
    }
    let started_at = segments
        .iter()
        .fold(None, |a, s| earliest(a, s.started_at.clone()));
    conn.prepare_cached(
        "UPDATE sessions SET workstream_id = ?2, started_at = ?3 WHERE session_id = ?1",
    )?
    .execute(params![session_id, current, started_at])?;

    // A subagent or teammate joins the segment live at its first record:
    // the latest segment started by then. Its first record follows the
    // dispatch within moments, and timestamps also place teammates and
    // nested subagents, whose dispatch is not in this transcript. With no
    // timestamp yet it is starting now: the live (final) segment.
    let agents: Vec<(i64, Option<String>)> = conn
        .prepare_cached(
            "SELECT a.id, (SELECT min(e.timestamp) FROM events AS e
                 WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id)
             FROM agents AS a WHERE a.session_id = ?1 AND a.agent_id IS NOT NULL",
        )?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    for (agent, first) in agents {
        let index = match first {
            None => segments.len().checked_sub(1),
            Some(t) => Some(
                segments
                    .iter()
                    .rposition(|s| s.started_at.as_ref().is_some_and(|st| *st <= t))
                    .unwrap_or(0),
            ),
        };
        let segment = index.and_then(|i| kept.get(i));
        conn.prepare_cached("UPDATE agents SET segment_id = ?2 WHERE id = ?1")?
            .execute(params![agent, segment])?;
    }
    continuation(conn, session_id)
}

/// Links the orchestrators of every session on `session_id`'s process root,
/// predecessor to successor in start order.
fn continuation(conn: &Connection, session_id: &str) -> Result<()> {
    let chain: Vec<i64> = conn
        .prepare_cached(
            "SELECT a.id FROM sessions AS s
             JOIN sessions AS me ON me.process_root = s.process_root
             JOIN agents AS a ON a.session_id = s.session_id AND a.agent_id IS NULL
             WHERE me.session_id = ?1
             ORDER BY s.started_at IS NULL, s.started_at, s.session_id",
        )?
        .query_map([session_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    if let Some(first) = chain.first() {
        conn.prepare_cached("DELETE FROM edges WHERE kind = 'continuation' AND child = ?1")?
            .execute([first])?;
    }
    for pair in chain.windows(2) {
        conn.prepare_cached(
            "INSERT INTO edges (kind, parent, child) VALUES ('continuation', ?1, ?2)
             ON CONFLICT (kind, child) DO UPDATE SET parent = excluded.parent",
        )?
        .execute(params![pair[0], pair[1]])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(offset: i64, reply: bool, title: Option<&str>) -> Row {
        Row {
            offset,
            reply,
            timestamp: None,
            cwd: None,
            branch: None,
            title: title.map(str::to_string),
        }
    }

    fn starts(rows: &[Row]) -> Vec<(i64, Option<String>)> {
        split(rows)
            .into_iter()
            .map(|s| (s.start, s.title))
            .collect()
    }

    #[test]
    fn a_title_that_returns_after_another_starts_a_new_segment() {
        let rows = [
            row(0, true, Some("a")),
            row(10, true, Some("b")),
            row(20, true, Some("a")),
        ];
        let a = Some("a".to_string());
        assert_eq!(
            starts(&rows),
            vec![(0, a.clone()), (10, Some("b".to_string())), (20, a)]
        );
    }

    #[test]
    fn a_run_of_replyless_segments_merges_into_the_next_with_a_reply() {
        let rows = [
            row(0, false, Some("a")),
            row(10, false, Some("b")),
            row(20, true, Some("c")),
        ];
        assert_eq!(starts(&rows), vec![(0, Some("c".to_string()))]);
    }

    #[test]
    fn dominant_ties_go_to_the_earliest_seen() {
        assert_eq!(dominant(["x", "y", "y", "x"].into_iter()), Some("x"));
        assert_eq!(dominant(["x", "y", "y"].into_iter()), Some("y"));
        assert_eq!(dominant(std::iter::empty()), None);
    }

    #[test]
    fn a_fallback_without_cwd_or_branch_uses_what_it_has() {
        let mut only_cwd = row(0, true, None);
        only_cwd.cwd = Some("/work/x".to_string());
        assert_eq!(fallback_name(&[only_cwd], "s1"), "/work/x");
        assert_eq!(fallback_name(&[row(0, true, None)], "s1"), "s1");
    }
}
