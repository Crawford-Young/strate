//! Enumerates a Claude Code config dir into a node/edge graph.
//!
//! Reads only `projects/` and `teams/` under the root. `.credentials.json`
//! and `sessions/` are never opened.

mod config_dir;
mod jsonl;
mod scan;

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub use config_dir::resolve_config_dir;
pub(crate) use jsonl::parse_line;
pub(crate) use scan::{is_transcript, transcript_paths};

/// Everything discovered under one config dir.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Graph {
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionNode>,
    pub subagents: Vec<SubagentNode>,
    pub teams: Vec<TeamNode>,
    /// Problems met while reading; discovery carries on past every one.
    pub warnings: Vec<Warning>,
}

/// One `projects/<dir>` entry. The dir name is a lossy encoding of the cwd
/// (or a `CLAUDE_CODE_PROJECT_DIR_NAME` override), so the real cwd lives on
/// each [`SessionNode`].
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub dir_name: String,
    pub path: PathBuf,
}

/// An orchestrator session: `projects/<dir>/<sessionId>.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionNode {
    pub session_id: String,
    pub project_dir: String,
    pub path: PathBuf,
    /// First `cwd` carried by any record (metadata records carry none).
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// Claude Code version (`version`) of the first record carrying one.
    pub version: Option<String>,
}

/// A subagent or teammate: `<sessionId>/subagents/agent-<agentId>.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentNode {
    pub session_id: String,
    pub agent_id: String,
    pub path: PathBuf,
    /// `None` when `agent-<agentId>.meta.json` is absent or unparseable.
    pub meta: Option<SubagentMeta>,
    pub link: Link,
}

/// Fields of `agent-<agentId>.meta.json`. Every field is optional: the key
/// set varies across Claude Code versions and between dispatched subagents
/// and teammates. Absent stays `None`; nothing is inherited or guessed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubagentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub spawn_depth: Option<u64>,
    pub tool_use_id: Option<String>,
    /// Teammate display name.
    pub name: Option<String>,
    /// Teammate team, matching a [`TeamNode::name`].
    pub team_name: Option<String>,
}

/// How a subagent node attaches to the rest of the graph.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// Dispatch edge: `parent`'s transcript holds an assistant `tool_use`
    /// block whose `id` equals the meta's `toolUseId`.
    Dispatch {
        parent: AgentRef,
        tool_use_id: String,
    },
    /// Teammate: meta has `teamName` and no `toolUseId`. `team_dir` is the
    /// `teams/<dir>` whose config `name` matches, when one exists.
    Teammate {
        team_name: String,
        team_dir: Option<String>,
    },
    /// The node is kept but its parent is unknown.
    Unresolved(Unresolved),
}

/// Why a subagent has no parent edge.
#[derive(Debug, Clone, PartialEq)]
pub enum Unresolved {
    /// No usable `.meta.json` beside the transcript (absent or unparseable).
    NoMeta,
    /// Meta present but carries neither `toolUseId` nor `teamName`.
    NoToolUseId,
    /// No transcript in the session holds a `tool_use` with this id.
    ToolUseNotFound { tool_use_id: String },
}

/// Identifies an agent node: the session's orchestrator or one subagent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentRef {
    Session {
        session_id: String,
    },
    Subagent {
        session_id: String,
        agent_id: String,
    },
}

/// `teams/<dir>/config.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamNode {
    pub dir_name: String,
    pub path: PathBuf,
    pub name: Option<String>,
    pub created_at: Option<serde_json::Number>,
    pub lead_agent_id: Option<String>,
    pub lead_session_id: Option<String>,
    /// Element shape is unverified, so members stay opaque.
    pub members: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    pub path: PathBuf,
    pub kind: WarningKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WarningKind {
    /// 1-based numbers of JSONL lines that failed to parse.
    MalformedLines(Vec<usize>),
    /// A subagent transcript with no `.meta.json`.
    MissingMeta,
    /// A `.meta.json` or team `config.json` that is not a JSON object.
    MalformedJson,
    /// A file or dir that could not be read.
    Unreadable(String),
}

/// Discovers the graph under `root` (a Claude Code config dir). Fails only
/// when `root` itself cannot be read; everything below it degrades into
/// [`Graph::warnings`].
pub fn discover(root: &Path) -> io::Result<Graph> {
    scan::discover(root)
}
