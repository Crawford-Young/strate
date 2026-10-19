//! The Audit rollups (#26): a tree over `audit_cells`, plus a headline and
//! the performance read, on a reader connection.
//!
//! Levels: Repo > Epic > Issue > PR > Session > Agent. A Session node is a
//! `/clear` chain's `/rename` windows in one workstream (#63): every file
//! one process wrote across `/clear`s (its process root) under one name,
//! with one orchestrator node and every link's subagents. A chain renamed
//! into another workstream reads there as another row: a real store had
//! 35 of 62 continuation edges cross workstreams, a process reused for the
//! next ticket. A director's `--bg` unit (a row with a session in
//! `directs`) nests under the director's row, the one holding the window
//! that messaged it, and its work counts there, not under its own issue;
//! a unit of a unit nests a level deeper. Rows that direct each other (a
//! cycle) nest under the earliest-started of them, by its first window's
//! start, whatever order they resolve in (#66). Its name (`<repo>-<issue>[-sN]`, bare or behind
//! `ticket-` or `epic-`; see [`crate::names`]) places it under its repo and
//! issue. When the GitHub cache holds that issue, it also goes under each
//! epic above the issue, outermost first (an epic is one node, its own
//! windows under its "Direct work"), and under a PR: the issue's PR that
//! the newest `github_links` entry of any of the row's sessions names, else
//! the issue's only PR, else none. A row has one place, whichever of its
//! windows a link came from, so it is one node. An issue the cache doesn't
//! hold (no `gh`, a failed fetch) has `github: false`, no epic, PR or
//! progress, and is open. A row whose name gives no place (a director's)
//! can still sit under a repo, as a Session node beside its issues (#63):
//! the one repo all its unit rows that name one work in (a unit whose name
//! gives no place counting where it sits in turn, one placed nowhere
//! skipped), else (units in several repos, or none) the repo a
//! `director-<repo>` name names when the GitHub cache or a window name
//! knows it. The outermost row places a nest. Any other window that is
//! unnamed, has a fallback name or has a name that doesn't parse sits
//! under "Unlinked". Any other group-by puts one Group
//! node per value (local day, PR, agent type, model) above that hierarchy;
//! the PR group and the headline read a unit's own window, wherever its
//! row nests, and so does the repo filter (see [`AuditFilter`]).
//!
//! Every node sums the cells under it. $, tokens, agent-hours (subagent
//! wall) and the counts are additive at every level, and so are a node's $
//! and agent-hours split by local day, agent type and model (#54): the
//! slice for a key is what that key's Group node holds on the same path.
//! Sessions, agents and rotations count once, on the first cell in the
//! filter of each window or subagent: a window across midnight is a session
//! on its first day. Active and waiting come from the orchestrator cells,
//! which carry the session timeline, so they are additive from Session up.
//! An Agent node shows its own active and waiting time, which is not summed
//! into its session, since its agents run in parallel. Elapsed and epic
//! progress are per level. Cells with no activity, requests or subagent run
//! (a window sitting open overnight) are left out, so they put no node on a
//! day.
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
    AgentRow, Bottleneck, BottleneckDay, LaneCount, Outcome, OutcomeDay, Peak, Performance,
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
/// (`YYYY-MM-DD`) of windows in `repos` (repo slugs, `""` for the windows
/// in none; empty: every window). A window is in the repo its name parses
/// to, or a director's in the repo its row is placed under. A unit's cells
/// still nest under its director's row wherever that is placed, so a
/// director with units in two repos shows, under its own placement, when
/// either is filtered: holding only the filtered repo's units, and its own
/// cells only when its placement is filtered.
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
    /// Agent-hours: subagent wall time, each subagent's from its own
    /// transcript's first record to its last, run in the foreground or the
    /// background alike.
    pub agent_ms: i64,
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
    /// the last activity. A session's is its windows in range, an agent's its run.
    pub elapsed_ms: Option<i64>,
    /// Elapsed runs to the last activity: the issue, epic or PR is open.
    pub open: bool,
    /// On an epic, or an issue with sub-issues.
    pub progress: Option<Progress>,
    /// By local day.
    pub by_day: Vec<Slice>,
    /// By agent type: `orchestrator`, a subagent's or teammate's type
    /// (`teammate` for one spawned with none recorded), or `""` (unknown).
    pub by_agent_type: Vec<Slice>,
    /// By model (`""`: a cell with no requests).
    pub by_model: Vec<Slice>,
}

/// A node's $ and agent-hours under one key of a split, its slices in key
/// order.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slice {
    pub key: String,
    pub cost_usd: f64,
    pub agent_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditNode {
    pub kind: NodeKind,
    /// Unique among its siblings: a repo slug, an issue or PR number, a
    /// Session row's `<process root>/<workstream id>` (`session:<id>` for a
    /// session with no window), an agent id (`""`: the orchestrator) or a
    /// group value.
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
    /// The repos the range's windows are filed under, sorted, whatever the
    /// repo filter: what it can pick from.
    pub repos: Vec<String>,
    /// Some window in the range is in no repo (`""` in the repo filter).
    pub unlinked: bool,
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
        repos: cells.repos.iter().cloned().collect(),
        unlinked: cells.unlinked,
    })
}
