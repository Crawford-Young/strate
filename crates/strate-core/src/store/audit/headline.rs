//! The headline: issues closed as completed and merged PRs with work in the
//! filter, and what they cost.

use std::collections::HashMap;

use rusqlite::Connection;

use super::Headline;
use super::cells::{Cells, Place};
use super::gh::Gh;
use crate::names::parse_name;
use crate::store::Result;
use crate::time::parse_timestamp;

/// The headline over the cells in the filter: each issue closed as
/// completed and each merged PR that has work there, with that work's $.
/// An issue's elapsed is its whole life, from its first window ever.
pub(crate) fn headline(conn: &Connection, gh: &Gh, cells: &Cells) -> Result<Headline> {
    let mut issues: HashMap<(&str, i64), f64> = HashMap::new();
    let mut merged: HashMap<(&str, i64), f64> = HashMap::new();
    for cell in &cells.list {
        let Place::Linked { repo, issue, pr } = &cells.window(cell).place else {
            continue;
        };
        let usd = cell.share.cost_usd.unwrap_or(0.0);
        *issues.entry((repo, *issue)).or_default() += usd;
        // A PR closing several issues counts once, with all their $.
        if let Some(pr) = pr.and_then(|n| gh.pr(repo, *issue, n))
            && pr.merged_at.is_some()
        {
            *merged.entry((&pr.repo, pr.number)).or_default() += usd;
        }
    }
    let starts = first_windows(conn)?;
    let mut elapsed = Vec::new();
    let mut closed = Vec::new();
    for ((repo, number), usd) in issues {
        // Closed at a known time, as completed.
        let completed = gh.issue(repo, number).filter(|i| i.completed);
        let Some(at) = completed.and_then(|i| i.closed.flatten()) else {
            continue;
        };
        let start = starts.get(&(repo.to_string(), number));
        elapsed.extend(start.map(|start| (at - start).max(0)));
        closed.push(usd);
    }
    elapsed.sort_unstable();
    let n = elapsed.len();
    let mean =
        |usd: Vec<f64>| (!usd.is_empty()).then(|| usd.iter().sum::<f64>() / usd.len() as f64);
    Ok(Headline {
        issues_closed: closed.len() as i64,
        median_elapsed_ms: match n {
            0 => None,
            _ if n % 2 == 1 => Some(elapsed[n / 2]),
            _ => Some((elapsed[n / 2 - 1] + elapsed[n / 2]) / 2),
        },
        usd_per_closed_issue: mean(closed),
        usd_per_merged_pr: mean(merged.into_values().collect()),
        cloud_uncounted: true,
    })
}

/// The start of each issue's first user-named window ever, by (repo, issue).
fn first_windows(conn: &Connection) -> Result<HashMap<(String, i64), i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT w.name, g.started_at FROM segments AS g
         JOIN workstream_roots AS r ON r.id = g.workstream_id
         JOIN workstreams AS w ON w.id = r.root
         WHERE NOT w.fallback AND g.started_at IS NOT NULL",
    )?;
    let mut rows = stmt.query([])?;
    let mut first: HashMap<(String, i64), i64> = HashMap::new();
    while let Some(r) = rows.next()? {
        let (Some(parsed), Some(start)) = (
            parse_name(&r.get::<_, String>(0)?),
            parse_timestamp(&r.get::<_, String>(1)?),
        ) else {
            continue;
        };
        first
            .entry((parsed.repo, parsed.issue as i64))
            .and_modify(|s| *s = (*s).min(start))
            .or_insert(start);
    }
    Ok(first)
}
