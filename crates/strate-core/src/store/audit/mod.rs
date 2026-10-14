//! The Audit rollups (#26): a tree over `audit_cells`, plus a headline and
//! the performance read, on a reader connection.
//!
//! Levels: Repo > Epic > Issue > PR > Session > Agent. A Session node is one
//! `/rename` window. Its name (`<repo>-<issue>[-sN]`, bare or behind
//! `ticket-` or `epic-`; see [`crate::names`]) places it under its repo and
//! issue. When the GitHub cache holds that issue, it also goes under each
//! epic above the issue, outermost first (an epic is one node, its own
//! windows under its "Direct work"), and under a PR: the issue's
//! PR that the session's `github_links` names, else the issue's only PR,
//! else none. An issue the cache doesn't hold (no `gh`, a failed fetch) has
//! `github: false`, no epic, PR or progress, and is open. Only a window that
//! is unnamed, has a fallback name or has a name that doesn't parse sits
//! under "Unlinked". Any other group-by puts one Group node per value (local
//! day, PR, agent type, model) above that hierarchy.
//!
//! Every node sums the cells under it. $, tokens, agent-hours (subagent
//! wall) and the counts are additive at every level. Sessions, agents and
//! rotations count once, on the first cell in the filter of each window or
//! subagent: a window across midnight is a session on its first day. Active
//! and waiting come from the orchestrator cells, which carry the session
//! timeline, so they are additive from Session up. An Agent node shows its
//! own active and waiting time, which is not summed into its session, since
//! its agents run in parallel. Elapsed and epic progress are per level.
//! Cells with no activity, requests or subagent run (a window sitting open
//! overnight) are left out, so they put no node on a day.
//!
//! Modules: `gh` reads the GitHub cache, `cells` the cells in a filter and
//! the windows and subagents they belong to, `tree` builds each cell's path
//! and the rollup tree over them, `headline` the closed-issue and merged-PR
//! figures, `timeline` one node's per-day spans and marks, and
//! `performance` the outcome, agent, bottleneck and waste measures (#51).

mod cells;
mod gh;
mod headline;
mod performance;
mod timeline;
mod tree;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::Result;
use cells::Cells;
use gh::Gh;
pub use performance::{
    AgentRow, Bottleneck, BottleneckDay, LaneCount, Outcome, OutcomeWeek, Peak, Performance,
    PermissionWaits, Waste, performance,
};
pub use timeline::{SpanKind, TimelineDay, TimelineMark, TimelineSpan, audit_timeline};

/// How the tree's top level groups the cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    Hierarchy,
    Day,
    Pr,
    AgentType,
    Model,
}

/// What [`audit`] rolls up: the cells on local days `from..=to`
/// (`YYYY-MM-DD`) of windows in `repos` (repo slugs; empty: every window,
/// Unlinked included).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditFilter {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub repos: Vec<String>,
    pub group_by: GroupBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Root,
    /// A value of the group-by: a day, `<repo>#<pr>` (the PR's own repo), an
    /// agent type or a model (`""`: none).
    Group,
    Repo,
    Unlinked,
    Epic,
    Issue,
    Pr,
    Session,
    Agent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    /// `cache_creation_input_tokens`: the whole write.
    pub cache_write: i64,
}

/// An epic's closed and total sub-issues, from GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub completed: i64,
    pub total: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    /// Priced requests; `unpriced` counts the rest.
    pub cost_usd: f64,
    pub unpriced: i64,
    pub tokens: Tokens,
    /// Agent-hours: subagent wall time.
    pub agent_ms: i64,
    /// Some of `agent_ms` is a background subagent's: its dispatch launched
    /// async (#55), so it ran beside its orchestrator instead of blocking
    /// it. Its hours are measured as a foreground one's are, from its own
    /// transcript's first record to its last (#42 changed only the live
    /// canvas state, not this).
    pub approximate: bool,
    pub active_ms: i64,
    /// Waiting on you.
    pub wait_ms: i64,
    /// Windows.
    pub sessions: i64,
    /// Subagents.
    pub agents: i64,
    /// Windows named `-sN`.
    pub rotations: i64,
    /// First window start to the close (epic, issue) or merge (PR), else to
    /// the last activity. A session's is its window, an agent's its run.
    pub elapsed_ms: Option<i64>,
    /// Elapsed runs to the last activity: the issue, epic or PR is open.
    pub open: bool,
    /// On an epic, or an issue with sub-issues.
    pub progress: Option<Progress>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditNode {
    pub kind: NodeKind,
    /// Unique among its siblings: a repo slug, an issue or PR number, a
    /// window's segment id (`session:<id>` with none), an agent id (`""`:
    /// the orchestrator) or a group value.
    pub key: String,
    pub label: String,
    /// An epic, issue or PR read from the GitHub cache. False on an issue
    /// known only from a window name, and on every other kind.
    pub github: bool,
    pub metrics: Metrics,
    pub children: Vec<AuditNode>,
}

/// The issues closed as completed and the merged PRs with work in the
/// filter, whatever the group-by.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Headline {
    pub issues_closed: i64,
    /// Over each issue's whole life, first window ever to close, even when
    /// the filter holds only part of it.
    pub median_elapsed_ms: Option<i64>,
    /// Their $ in the filter over their count.
    pub usd_per_closed_issue: Option<f64>,
    /// Their $ in the filter over their count; a PR closing several issues
    /// counts once.
    pub usd_per_merged_pr: Option<f64>,
    /// Cloud sessions are not in the store (#29), so their spend is missing.
    pub cloud_uncounted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Audit {
    pub root: AuditNode,
    pub headline: Headline,
    /// The repos whose GitHub data is missing or stale: their issues show
    /// no title, epic or PR.
    pub github_errors: Vec<GithubError>,
}

/// A repo (slug) whose latest GitHub refresh failed, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubError {
    pub repo: String,
    pub error: String,
}

/// The rollup tree and headline for `filter`. Fails on a day that is not
/// `YYYY-MM-DD`.
pub fn audit(conn: &Connection, filter: &AuditFilter) -> Result<Audit> {
    let gh = Gh::load(conn)?;
    let cells = Cells::load(conn, &gh, filter)?;
    Ok(Audit {
        root: tree::tree(filter.group_by, &cells, &gh),
        headline: headline::headline(conn, &gh, &cells)?,
        github_errors: gh::github_errors(conn)?,
    })
}
