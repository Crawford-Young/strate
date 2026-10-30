//! The headline: issues closed as completed and merged PRs with work in the
//! filter, and what they cost.

use std::collections::HashMap;

use super::Headline;
use super::cells::Cells;
use super::gh::Gh;

/// The headline over the cells in the filter: each issue closed as
/// completed and each merged PR that has work there, with that work's $.
/// An issue's elapsed is its whole life, from the first session ever
/// charged to it.
pub(crate) fn headline(gh: &Gh, cells: &Cells) -> Headline {
    let mut issues: HashMap<(&str, i64), f64> = HashMap::new();
    let mut merged: HashMap<(&str, i64), f64> = HashMap::new();
    for cell in &cells.list {
        let Some((repo, issue, pr)) = cells.row_of(cell).place.issue() else {
            continue;
        };
        let usd = cell.share.cost_usd.unwrap_or(0.0);
        *issues.entry((repo, issue)).or_default() += usd;
        // A PR closing several issues counts once, with all their $.
        if let Some(pr) = pr.and_then(|n| gh.pr(repo, issue, n))
            && pr.merged_at.is_some()
        {
            *merged.entry((&pr.repo, pr.number)).or_default() += usd;
        }
    }
    let starts = cells.issue_starts(gh);
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
    Headline {
        issues_closed: closed.len() as i64,
        median_elapsed_ms: match n {
            0 => None,
            _ if n % 2 == 1 => Some(elapsed[n / 2]),
            _ => Some((elapsed[n / 2 - 1] + elapsed[n / 2]) / 2),
        },
        usd_per_closed_issue: mean(closed),
        usd_per_merged_pr: mean(merged.into_values().collect()),
        cloud_uncounted: true,
    }
}
