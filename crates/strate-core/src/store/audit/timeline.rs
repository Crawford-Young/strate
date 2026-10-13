//! One node's day timeline for the Audit row detail: when its work ran and
//! when its issues closed and PRs merged, per local day.
//!
//! The node is resolved as the tree resolves it: the cells in the filter
//! whose [`path`] passes through the node's steps. Their sessions' events
//! are replayed into runs cut where the cells are cut (local midnights, and
//! window starts on the orchestrator's line), so a span sits in exactly one
//! cell and a cell's time is the sum of its spans.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use super::cells::{Cell, Cells};
use super::gh::Gh;
use super::tree::Step;
use super::{AuditFilter, NodeKind};
use crate::store::Result;
use crate::store::account::{self, Run};
use crate::time::{self, Cat};

/// What a span of a timeline is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// The orchestrator at work: not idle, not waiting on you.
    Active,
    /// The orchestrator waiting on you: a question, or the next prompt.
    Wait,
    /// A subagent at work (not idle). Parallel ones overlap.
    Agent,
}

/// `[start_ms, end_ms)` in ms since its day's local midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSpan {
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: SpanKind,
}

/// An issue's close (`#N closed`) or a PR's merge (`PR #N merged`), in ms
/// since its day's local midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineMark {
    pub at_ms: i64,
    pub label: String,
}

/// One local day (`YYYY-MM-DD`) of a node's timeline: spans by start, marks
/// by time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDay {
    pub day: String,
    pub spans: Vec<TimelineSpan>,
    pub marks: Vec<TimelineMark>,
}

/// A cell's time: its session, agent (`None`: the orchestrator), window
/// and day.
type CellKey<'a> = (&'a str, Option<&'a str>, Option<i64>, &'a str);

/// The days in `filter` with work or a mark of the node at `path` (its
/// steps from the root, which may lead with the root's own), under
/// `filter.group_by`. An unknown path has no days. Fails on a day that is
/// not `YYYY-MM-DD`.
pub fn audit_timeline(
    conn: &Connection,
    filter: &AuditFilter,
    path: &[(NodeKind, String)],
) -> Result<Vec<TimelineDay>> {
    let path = match path {
        [(NodeKind::Root, _), rest @ ..] => rest,
        _ => path,
    };
    let gh = Gh::load(conn)?;
    let cells = Cells::load(conn, &gh, filter)?;
    let tz = account::zone(conn)?;
    let mut timed: HashSet<CellKey> = HashSet::new();
    let mut marks: BTreeSet<(i64, String)> = BTreeSet::new();
    for cell in &cells.list {
        let steps = super::tree::path(filter.group_by, cell, &cells, &gh);
        if !under(&steps, path) {
            continue;
        }
        if has_time(cell) {
            timed.insert((
                &cell.session_id,
                cell.agent_id.as_deref(),
                cell.segment,
                &cell.day,
            ));
        }
        // The node's own step and those under it.
        let at_or_under = &steps[path.len().saturating_sub(1)..];
        marks.extend(at_or_under.iter().filter_map(mark));
    }

    let mut days: BTreeMap<String, TimelineDay> = BTreeMap::new();
    let sessions: BTreeSet<&str> = timed.iter().map(|k| k.0).collect();
    for session in sessions {
        for run in account::session_runs(conn, session, &tz)? {
            let Some(kind) = kind(&run) else { continue };
            let (Some(day), Some(midnight)) = (&run.day, time::day_start(&tz, run.start)) else {
                continue;
            };
            let key = (session, run.agent.as_deref(), run.segment, day.as_str());
            if timed.contains(&key) {
                entry(&mut days, day).spans.push(TimelineSpan {
                    start_ms: run.start - midnight,
                    end_ms: run.end - midnight,
                    kind,
                });
            }
        }
    }
    for (at, label) in marks {
        let (Some(day), Some(midnight)) = (time::local_day(&tz, at), time::day_start(&tz, at))
        else {
            continue;
        };
        if (filter.from.as_str()..=filter.to.as_str()).contains(&day.as_str()) {
            entry(&mut days, &day).marks.push(TimelineMark {
                at_ms: at - midnight,
                label,
            });
        }
    }
    Ok(days
        .into_values()
        .map(|mut d| {
            d.spans = merged(d.spans);
            d
        })
        .collect())
}

/// Whether a cell's path passes through every step of `node`.
fn under(steps: &[Step], node: &[(NodeKind, String)]) -> bool {
    steps.len() >= node.len()
        && steps
            .iter()
            .zip(node)
            .all(|(s, (kind, key))| s.kind == *kind && s.key == *key)
}

/// Whether a cell carries its bucket's time: a bucket's time is all on one
/// of its cells (its dominant model's), so the rest add no spans.
fn has_time(cell: &Cell) -> bool {
    let s = &cell.share;
    s.own.active_ms > 0 || s.own.wait_ms > 0 || s.agent_ms > 0
}

/// A closed epic or issue, or a merged PR, as its mark.
fn mark(step: &Step) -> Option<(i64, String)> {
    let at = step.closed_at()?;
    match step.kind {
        NodeKind::Epic | NodeKind::Issue => Some((at, format!("#{} closed", step.key))),
        NodeKind::Pr => Some((at, format!("PR #{} merged", step.key))),
        _ => None,
    }
}

/// How a run shows: idle shows nothing; a subagent's other time is agent
/// time; the orchestrator's is waiting on a question or prompt, else active.
fn kind(run: &Run) -> Option<SpanKind> {
    match (run.cat, &run.agent) {
        (Cat::Idle, _) => None,
        (_, Some(_)) => Some(SpanKind::Agent),
        (Cat::Ask | Cat::Wait, None) => Some(SpanKind::Wait),
        (_, None) => Some(SpanKind::Active),
    }
}

fn entry<'a>(days: &'a mut BTreeMap<String, TimelineDay>, day: &str) -> &'a mut TimelineDay {
    days.entry(day.to_string()).or_insert_with(|| TimelineDay {
        day: day.to_string(),
        spans: Vec::new(),
        marks: Vec::new(),
    })
}

/// `spans` by start, each kind's touching neighbours as one span. Spans that
/// overlap stay apart, so parallel work keeps its sum.
fn merged(mut spans: Vec<TimelineSpan>) -> Vec<TimelineSpan> {
    spans.sort_by_key(|s| (s.kind, s.start_ms, s.end_ms));
    let mut out: Vec<TimelineSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(last) if last.kind == span.kind && last.end_ms == span.start_ms => {
                last.end_ms = span.end_ms;
            }
            _ => out.push(span),
        }
    }
    out.sort_by_key(|s| (s.start_ms, s.end_ms, s.kind));
    out
}
