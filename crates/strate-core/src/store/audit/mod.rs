//! The Audit rollups (#26): a tree over `audit_cells`, plus a headline and
//! the performance read, on a reader connection.
//!
//! Each cell is one agent's work on one local day and model, and it sits
//! where its agent's effective charge (`agent_charges`, #107) puts it:
//!
//! - issue: Repo > Epic (each epic above it in the GitHub cache, outermost
//!   first) > Issue > PR > Session > Agent. An epic's own issue charge is
//!   one Epic node with its work under a "Direct work" Issue node. The PR
//!   is the issue's PR that the newest `github_links` entry of the Session
//!   row's sessions names, else the issue's only PR, else none.
//! - epic: Repo > Epic (and the epics above it) > Session > Agent.
//! - pr: under the issue it closes when the cache holds it (the lowest
//!   issue of its repo), as an issue's work under that PR; else Repo > PR.
//! - repo: Repo > Session > Agent.
//! - topic: a top-level Topic node per topic > Session > Agent.
//! - none: Unlinked > Session > Agent.
//!
//! An issue the cache doesn't hold (no `gh`, a failed fetch) has `github:
//! false`, no epic, PR or progress, and is open; so is a PR it doesn't hold.
//! A Session row is a `/clear` chain (every file one process wrote, by its
//! process root) within one charge: its sessions' orchestrators and
//! subagents of that charge. A subagent charged apart from its orchestrator
//! sits under its own charge, in a Session row of its session holding only
//! it. A session another one messaged or launched has its own charge, or
//! inherits it, and nests under nobody. The repo filter reads a cell by its
//! charge's repo (see [`AuditFilter`]).
//!
//! Every node sums the cells under it. $, tokens, agent-hours (subagent
//! wall) and the counts are additive at every level, and so are a node's $
//! and agent-hours split by local day, agent type and model (#54): the
//! slice for a key is what the node holds of that key's cells. Sessions
//! count once per charge, on the first cell in the filter of each (session,
//! charge); agents on the first cell of each subagent. Active and waiting
//! come from the orchestrator cells, which carry the session timeline, so
//! they are additive from Session up. An Agent node shows its own active
//! and waiting time, which is not summed into its session, since its agents
//! run in parallel. Elapsed and epic progress are per level. Cells with no
//! activity, requests or subagent run (a session sitting open overnight)
//! are left out, so they put no node on a day.
//!
//! Modules: `gh` reads the GitHub cache, `cells` the cells in a filter and
//! the agents, sessions and Session rows they belong to, `tree` builds
//! each cell's path and the rollup tree over them, `headline` the
//! closed-issue and merged-PR figures, `timeline` one node's per-day spans
//! and marks, and `performance` the outcome, agent, bottleneck and waste
//! measures (#51).

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

/// What [`audit`] rolls up: the cells on local days `from..=to`
/// (`YYYY-MM-DD`) of agents charged in `repos` (repo slugs, `""` for a
/// topic or no charge; empty: every agent). A cell is in its agent's
/// effective charge's repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditFilter {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Root,
    Repo,
    /// A topic charge: work on no repo.
    Topic,
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
    /// Sessions, each once per charge its agents work.
    pub sessions: i64,
    /// Subagents.
    pub agents: i64,
    /// First session start to the close (epic, issue) or merge (PR), else
    /// to the last activity. A Session row's is the time its sessions (or,
    /// alone in the row, its subagents) cover in range, an agent's its run.
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
    /// Unique among its siblings: a repo slug, a topic, an issue or PR
    /// number, a Session row's `<process root>/<charge key>` (`<process
    /// root>/` with no charge) or an agent id (`""`: the orchestrator).
    pub key: String,
    pub label: String,
    /// An epic, issue or PR read from the GitHub cache. False on one known
    /// only from a charge, and on every other kind.
    pub github: bool,
    pub metrics: Metrics,
    pub children: Vec<AuditNode>,
}

/// The issues closed as completed and the merged PRs with work in the
/// filter.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Headline {
    pub issues_closed: i64,
    /// Over each issue's whole life, from the start of the first session
    /// ever charged to it, even when the filter holds only part of it.
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
    /// The repos the range's cells are charged in, sorted, whatever the
    /// repo filter: what it can pick from.
    pub repos: Vec<String>,
    /// Some cell in the range is in no repo (`""` in the repo filter).
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
        root: tree::tree(&cells, &gh),
        headline: headline::headline(&gh, &cells),
        github_errors: gh::github_errors(conn)?,
        repos: cells.repos.iter().cloned().collect(),
        unlinked: cells.unlinked,
    })
}
