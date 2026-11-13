use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::tail::FileIdentity;

use super::jsonl::parse_line;
use super::{
    AgentRef, Graph, Link, Project, SessionNode, SubagentMeta, SubagentNode, TeamNode, Unresolved,
    Warning, WarningKind,
};

/// Discovers `root` with one process-wide [`Scanner`].
pub(super) fn discover(root: &Path) -> io::Result<Graph> {
    static SCANNER: LazyLock<Mutex<Scanner>> = LazyLock::new(Mutex::default);
    let mut scanner = SCANNER.lock().unwrap_or_else(|poisoned| {
        // A pass that panicked may have left its cache half-updated.
        let mut scanner = poisoned.into_inner();
        *scanner = Scanner::default();
        scanner
    });
    SCANNER.clear_poison();
    scanner.discover(root)
}

/// Discovers a config dir pass after pass, keeping what each transcript's
/// records give a pass, so a pass costs what changed since the last (#121):
/// a transcript whose size and mtime are unchanged is not opened, one that
/// grew is read from its last complete line, and one that shrank, was
/// replaced (another file identity) or kept its size under a newer mtime is
/// read whole, as the tailer does. Metas
/// and team configs are small and read every pass, so a meta's `finished`
/// is as fresh as a full scan's.
#[derive(Debug, Default)]
pub struct Scanner {
    /// The transcripts the last pass read, by path.
    last: HashMap<PathBuf, Cached>,
    /// The transcripts this pass has read.
    next: HashMap<PathBuf, Cached>,
    bytes_read: u64,
}

#[derive(Debug)]
struct Cached {
    len: u64,
    modified: Option<SystemTime>,
    identity: Option<FileIdentity>,
    transcript: Transcript,
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Transcript bytes read by every pass so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Discovers the graph under `root`, as [`super::discover`] documents.
    pub fn discover(&mut self, root: &Path) -> io::Result<Graph> {
        fs::read_dir(root)?;
        let mut g = Graph::default();
        g.teams = read_teams(&root.join("teams"), &mut g.warnings);
        for path in sorted_entries(&root.join("projects"), &mut g.warnings) {
            if path.is_dir() {
                let dir_name = name_of(&path);
                self.scan_project(&path, &dir_name, &mut g);
                g.projects.push(Project { dir_name, path });
            }
        }
        // A transcript gone since is forgotten.
        self.last = std::mem::take(&mut self.next);
        Ok(g)
    }

    fn scan_project(&mut self, project: &Path, dir_name: &str, g: &mut Graph) {
        let mut session_ids: Vec<String> = sorted_entries(project, &mut g.warnings)
            .iter()
            .filter_map(|p| {
                let name = name_of(p);
                if p.is_dir() {
                    Some(name)
                } else {
                    name.strip_suffix(".jsonl").map(str::to_string)
                }
            })
            .collect();
        session_ids.sort();
        session_ids.dedup();
        for session_id in session_ids {
            self.scan_session(project, dir_name, &session_id, g);
        }
    }

    fn scan_session(&mut self, project: &Path, dir_name: &str, session_id: &str, g: &mut Graph) {
        // tool_use id -> the agent whose transcript holds it (first wins).
        let mut tool_uses: HashMap<String, AgentRef> = HashMap::new();

        let transcript = project.join(format!("{session_id}.jsonl"));
        if transcript.is_file() {
            let owner = AgentRef::Session {
                session_id: session_id.to_string(),
            };
            let mut node = SessionNode {
                session_id: session_id.to_string(),
                project_dir: dir_name.to_string(),
                path: transcript.clone(),
                cwd: None,
                git_branch: None,
                version: None,
            };
            if let Some(t) = self.read(&transcript, &mut g.warnings) {
                node.cwd = t.first(|r| &r.cwd);
                node.git_branch = t.first(|r| &r.git_branch);
                node.version = t.first(|r| &r.version);
                t.add_tool_uses(&owner, &mut tool_uses);
            }
            g.sessions.push(node);
        }

        let mut pending = Vec::new();
        for path in sorted_entries(&project.join(session_id).join("subagents"), &mut g.warnings) {
            let file = name_of(&path);
            let Some(agent_id) = agent_id_of(&file) else {
                continue;
            };
            let owner = AgentRef::Subagent {
                session_id: session_id.to_string(),
                agent_id: agent_id.to_string(),
            };
            if let Some(t) = self.read(&path, &mut g.warnings) {
                t.add_tool_uses(&owner, &mut tool_uses);
            }
            let meta = read_meta(&path, agent_id, &mut g.warnings);
            pending.push((agent_id.to_string(), owner, path, meta));
        }

        // Links resolve only after every transcript in the session is read, so
        // a nested subagent finds a parent subagent listed after it.
        let siblings: HashSet<String> = pending.iter().map(|(id, ..)| id.clone()).collect();
        for (agent_id, owner, path, meta) in pending {
            let link = resolve_link(meta.as_ref(), &owner, &tool_uses, &g.teams, &siblings);
            g.subagents.push(SubagentNode {
                session_id: session_id.to_string(),
                agent_id,
                path,
                meta,
                link,
            });
        }
    }

    /// What the transcript at `path` holds now, warning when it is
    /// unreadable or has lines that do not parse.
    fn read(&mut self, path: &Path, warnings: &mut Vec<Warning>) -> Option<&Transcript> {
        let cached = self.last.remove(path);
        let read = fs::metadata(path).and_then(|m| {
            let (len, modified) = (m.len(), m.modified().ok());
            let cached = match cached {
                Some(c) if c.len == len && c.modified == modified => return Ok(c),
                cached => cached,
            };
            let identity = FileIdentity::of(path).ok();
            let mut transcript = match cached {
                // Same size but another mtime is a rewrite, not growth.
                Some(c) if identity.is_some() && c.identity == identity && len > c.len => {
                    c.transcript
                }
                _ => Transcript::default(),
            };
            transcript.read_from(path, &mut self.bytes_read)?;
            Ok(Cached {
                len,
                modified,
                identity,
                transcript,
            })
        });
        let cached = match read {
            Ok(cached) => cached,
            Err(e) => {
                warn(warnings, path, WarningKind::Unreadable(e.to_string()));
                return None;
            }
        };
        self.next.insert(path.to_path_buf(), cached);
        let t = &self.next[path].transcript;
        t.warn(path, warnings);
        Some(t)
    }
}

/// What one transcript's records give a pass.
#[derive(Debug, Default)]
struct Records {
    cwd: Option<String>,
    git_branch: Option<String>,
    version: Option<String>,
    tool_uses: Vec<String>,
    /// 1-based numbers of the lines that fail to parse.
    bad: Vec<usize>,
}

impl Records {
    fn add(&mut self, line_no: usize, line: &[u8]) {
        match parse_line(line) {
            None => {}
            Some(Ok(record)) => {
                fill_first(&mut self.cwd, &record, "cwd");
                fill_first(&mut self.git_branch, &record, "gitBranch");
                fill_first(&mut self.version, &record, "version");
                self.tool_uses.extend(tool_use_ids(&record));
            }
            Some(Err(_)) => self.bad.push(line_no),
        }
    }
}

/// One transcript as read so far.
#[derive(Debug, Default)]
struct Transcript {
    /// Bytes through the last complete line, and the lines in them.
    offset: u64,
    lines: usize,
    complete: Records,
    /// The line after `offset` that has no newline yet.
    partial: Records,
}

impl Transcript {
    /// Reads `path` from `offset` to its end, counting the bytes read.
    fn read_from(&mut self, path: &Path, bytes_read: &mut u64) -> io::Result<()> {
        let mut reader = BufReader::new(File::open(path)?);
        reader.seek(SeekFrom::Start(self.offset))?;
        self.partial = Records::default();
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                return Ok(());
            }
            *bytes_read += n as u64;
            if line.ends_with(b"\n") {
                self.lines += 1;
                self.complete.add(self.lines, &line);
                self.offset += n as u64;
            } else {
                self.partial.add(self.lines + 1, &line);
            }
        }
    }

    /// The first value `field` picks from a record.
    fn first(&self, field: impl Fn(&Records) -> &Option<String>) -> Option<String> {
        field(&self.complete)
            .as_ref()
            .or(field(&self.partial).as_ref())
            .cloned()
    }

    fn add_tool_uses(&self, owner: &AgentRef, out: &mut HashMap<String, AgentRef>) {
        let ids = self.complete.tool_uses.iter();
        for id in ids.chain(&self.partial.tool_uses) {
            if !out.contains_key(id) {
                out.insert(id.clone(), owner.clone());
            }
        }
    }

    fn warn(&self, path: &Path, warnings: &mut Vec<Warning>) {
        let bad: Vec<usize> = [&self.complete.bad, &self.partial.bad]
            .into_iter()
            .flatten()
            .copied()
            .collect();
        if !bad.is_empty() {
            warn(warnings, path, WarningKind::MalformedLines(bad));
        }
    }
}

/// Every transcript (session or subagent `.jsonl`) under `root/projects`,
/// sorted, found by listing dirs only: no file is opened.
pub(crate) fn transcript_paths(root: &Path) -> Vec<PathBuf> {
    let mut ignored = Vec::new();
    let mut out = Vec::new();
    for project in sorted_entries(&root.join("projects"), &mut ignored) {
        for entry in sorted_entries(&project, &mut ignored) {
            if entry.is_dir() {
                out.extend(sorted_entries(&entry.join("subagents"), &mut ignored));
            } else {
                out.push(entry);
            }
        }
    }
    out.retain(|p| is_transcript(root, p) && p.is_file());
    out.sort();
    out
}

/// Whether `path` sits where a transcript lives under `root/projects`:
/// `<project>/<sessionId>.jsonl` or
/// `<project>/<sessionId>/subagents/agent-<agentId>.jsonl`.
pub(crate) fn is_transcript(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root.join("projects")) else {
        return false;
    };
    let parts: Vec<&str> = rel
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or_default())
        .collect();
    match parts.as_slice() {
        [_, file] => file.ends_with(".jsonl"),
        [_, _, "subagents", file] => agent_id_of(file).is_some(),
        _ => false,
    }
}

/// `agent-<agentId>.jsonl` -> `agentId`.
fn agent_id_of(file: &str) -> Option<&str> {
    file.strip_prefix("agent-")
        .and_then(|rest| rest.strip_suffix(".jsonl"))
}

/// Entries of `dir`, sorted. A missing dir is simply empty; any other read
/// failure becomes a warning.
fn sorted_entries(dir: &Path, warnings: &mut Vec<Warning>) -> Vec<PathBuf> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut paths: Vec<PathBuf> =
                entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            paths.sort();
            paths
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            warn(warnings, dir, WarningKind::Unreadable(e.to_string()));
            Vec::new()
        }
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn warn(warnings: &mut Vec<Warning>, path: &Path, kind: WarningKind) {
    warnings.push(Warning {
        path: path.to_path_buf(),
        kind,
    });
}

fn fill_first(slot: &mut Option<String>, record: &Value, key: &str) {
    if slot.is_none() {
        *slot = record.get(key).and_then(Value::as_str).map(str::to_string);
    }
}

/// The ids of an assistant record's `tool_use` blocks.
fn tool_use_ids(record: &Value) -> impl Iterator<Item = String> + '_ {
    let blocks = match record.get("type").and_then(Value::as_str) {
        Some("assistant") => record.pointer("/message/content").and_then(Value::as_array),
        _ => None,
    };
    blocks.into_iter().flatten().filter_map(|block| {
        (block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .then(|| block.get("id").and_then(Value::as_str))
            .flatten()
            .map(str::to_string)
    })
}

/// Reads JSON at `path` as an object, warning when it is unreadable or not
/// an object.
fn read_object(path: &Path, warnings: &mut Vec<Warning>) -> Option<Map<String, Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            warn(warnings, path, WarningKind::Unreadable(e.to_string()));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Some(map),
        _ => {
            warn(warnings, path, WarningKind::MalformedJson);
            None
        }
    }
}

/// The meta `finished` of the subagent whose transcript is `transcript`,
/// read now: a linked subagent's records start no discovery pass, so its
/// batch reads it too (#108). `None` when the meta is missing, unreadable or says nothing.
pub(crate) fn meta_finished(transcript: &Path, agent_id: &str) -> Option<bool> {
    meta_of(transcript, agent_id)?.finished
}

/// The meta of the subagent whose transcript is `transcript`, read now,
/// with no warning; `None` when it is missing or unreadable.
pub(crate) fn meta_of(transcript: &Path, agent_id: &str) -> Option<SubagentMeta> {
    read_meta(transcript, agent_id, &mut Vec::new())
}

fn read_meta(
    transcript: &Path,
    agent_id: &str,
    warnings: &mut Vec<Warning>,
) -> Option<SubagentMeta> {
    let path = transcript.with_file_name(format!("agent-{agent_id}.meta.json"));
    if !path.exists() {
        warn(warnings, transcript, WarningKind::MissingMeta);
        return None;
    }
    let map = read_object(&path, warnings)?;
    let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_string);
    Some(SubagentMeta {
        agent_type: text("agentType"),
        spawned_agent_type: text("spawnedAgentType").or_else(|| text("customAgentType")),
        description: text("description"),
        model: text("model"),
        effort: map.get("effort").and_then(scalar_text),
        spawn_depth: map.get("spawnDepth").and_then(Value::as_u64),
        tool_use_id: text("toolUseId"),
        parent_agent_id: text("parentAgentId"),
        name: text("name"),
        team_name: text("teamName"),
        finished: map.get("finished").and_then(Value::as_bool),
    })
}

/// `effort`'s type is unverified: keep strings as-is and render other
/// scalars as text rather than dropping them.
pub(crate) fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

/// How `owner` (a subagent) attaches, from its meta: the agent whose
/// transcript holds its `toolUseId`, else its team, else the sibling (one of
/// `siblings`, its session's subagent ids) its `parentAgentId` names.
fn resolve_link(
    meta: Option<&SubagentMeta>,
    owner: &AgentRef,
    tool_uses: &HashMap<String, AgentRef>,
    teams: &[TeamNode],
    siblings: &HashSet<String>,
) -> Link {
    let Some(meta) = meta else {
        return Link::Unresolved(Unresolved::NoMeta);
    };
    let found = meta
        .tool_use_id
        .as_ref()
        .and_then(|id| tool_uses.get(id).filter(|p| *p != owner).map(|p| (id, p)));
    if let Some((tool_use_id, parent)) = found {
        return Link::Dispatch {
            parent: parent.clone(),
            tool_use_id: tool_use_id.clone(),
        };
    }
    let spawner = match (owner, &meta.parent_agent_id, &meta.team_name) {
        (
            AgentRef::Subagent {
                session_id,
                agent_id,
            },
            Some(parent),
            None,
        ) if parent != agent_id && siblings.contains(parent) => Some(AgentRef::Subagent {
            session_id: session_id.clone(),
            agent_id: parent.clone(),
        }),
        _ => None,
    };
    if let Some(parent) = spawner {
        return Link::ParentAgent { parent };
    }
    if let Some(tool_use_id) = &meta.tool_use_id {
        return Link::Unresolved(Unresolved::ToolUseNotFound {
            tool_use_id: tool_use_id.clone(),
        });
    }
    match &meta.team_name {
        Some(team_name) => Link::Teammate {
            team_name: team_name.clone(),
            team_dir: teams
                .iter()
                .find(|t| t.name.as_ref() == Some(team_name))
                .map(|t| t.dir_name.clone()),
        },
        None => Link::Unresolved(Unresolved::NoToolUseId),
    }
}

fn read_teams(teams_dir: &Path, warnings: &mut Vec<Warning>) -> Vec<TeamNode> {
    let mut teams = Vec::new();
    for dir in sorted_entries(teams_dir, warnings) {
        let path = dir.join("config.json");
        if !path.is_file() {
            continue;
        }
        let Some(map) = read_object(&path, warnings) else {
            continue;
        };
        let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_string);
        teams.push(TeamNode {
            dir_name: name_of(&dir),
            name: text("name"),
            created_at: match map.get("createdAt") {
                Some(Value::Number(n)) => Some(n.clone()),
                _ => None,
            },
            lead_agent_id: text("leadAgentId"),
            lead_session_id: text("leadSessionId"),
            members: map
                .get("members")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            path,
        });
    }
    teams
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(tool_use_id: Option<&str>, team_name: Option<&str>) -> SubagentMeta {
        SubagentMeta {
            tool_use_id: tool_use_id.map(str::to_string),
            team_name: team_name.map(str::to_string),
            ..SubagentMeta::default()
        }
    }

    fn sub(agent_id: &str) -> AgentRef {
        AgentRef::Subagent {
            session_id: "s".to_string(),
            agent_id: agent_id.to_string(),
        }
    }

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home")
    }

    #[test]
    fn transcript_paths_lists_every_session_and_subagent_jsonl() {
        let root = fixture_root();
        let got = transcript_paths(&root);
        let g = discover(&root).expect("fixture root");
        let mut expected: Vec<PathBuf> = g
            .sessions
            .iter()
            .map(|s| s.path.clone())
            .chain(g.subagents.iter().map(|s| s.path.clone()))
            .collect();
        expected.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn transcript_paths_of_a_root_without_projects_is_empty() {
        assert!(transcript_paths(&fixture_root().join("teams")).is_empty());
    }

    #[test]
    fn is_transcript_accepts_only_the_transcript_layout() {
        let root = Path::new("/cfg");
        let yes = [
            "projects/-work-x/s1.jsonl",
            "projects/-work-x/s1/subagents/agent-a1.jsonl",
        ];
        let no = [
            ".credentials.json",
            "sessions/123.json",
            "projects/s1.jsonl",
            "projects/-work-x/s1.json",
            "projects/-work-x/s1/subagents/agent-a1.meta.json",
            "projects/-work-x/s1/subagents/other.jsonl",
            "projects/-work-x/s1/tool-results/agent-a1.jsonl",
            "projects/-work-x/s1/subagents/deeper/agent-a1.jsonl",
            "teams/session-0/config.json",
        ];
        for rel in yes {
            assert!(is_transcript(root, &root.join(rel)), "{rel}");
        }
        for rel in no {
            assert!(!is_transcript(root, &root.join(rel)), "{rel}");
        }
        assert!(!is_transcript(
            root,
            Path::new("/elsewhere/projects/p/s.jsonl")
        ));
    }

    fn siblings() -> HashSet<String> {
        HashSet::from(["a".to_string(), "b".to_string()])
    }

    #[test]
    fn meta_without_tool_use_id_or_team_is_unresolved() {
        let link = resolve_link(
            Some(&meta(None, None)),
            &sub("a"),
            &HashMap::new(),
            &[],
            &siblings(),
        );
        assert_eq!(link, Link::Unresolved(Unresolved::NoToolUseId));
    }

    fn parented(tool_use_id: Option<&str>, team: Option<&str>, parent: &str) -> SubagentMeta {
        SubagentMeta {
            parent_agent_id: Some(parent.to_string()),
            ..meta(tool_use_id, team)
        }
    }

    #[test]
    fn a_parent_agent_id_links_a_subagent_whose_tool_use_places_it_nowhere() {
        let link = |m: &SubagentMeta, tool_uses: &HashMap<String, AgentRef>| {
            resolve_link(Some(m), &sub("a"), tool_uses, &[], &siblings())
        };
        let none = HashMap::new();
        for m in [
            parented(None, None, "b"),
            parented(Some("toolu_gone"), None, "b"),
        ] {
            assert_eq!(link(&m, &none), Link::ParentAgent { parent: sub("b") });
        }
        // A tool_use that resolves wins.
        let session = AgentRef::Session {
            session_id: "s".to_string(),
        };
        let tool_uses = HashMap::from([("toolu_x".to_string(), session.clone())]);
        assert_eq!(
            link(&parented(Some("toolu_x"), None, "b"), &tool_uses),
            Link::Dispatch {
                parent: session,
                tool_use_id: "toolu_x".to_string(),
            }
        );
        // Itself, or no subagent of the session: as without it.
        assert_eq!(
            link(&parented(None, None, "a"), &none),
            Link::Unresolved(Unresolved::NoToolUseId)
        );
        assert_eq!(
            link(&parented(Some("toolu_gone"), None, "zz"), &none),
            Link::Unresolved(Unresolved::ToolUseNotFound {
                tool_use_id: "toolu_gone".to_string(),
            })
        );
        // A teammate keeps its team link.
        assert!(matches!(
            link(&parented(None, Some("t"), "b"), &none),
            Link::Teammate { .. }
        ));
    }

    #[test]
    fn teammate_of_unknown_team_keeps_the_name_without_a_dir() {
        let link = resolve_link(
            Some(&meta(None, Some("ghost"))),
            &sub("a"),
            &HashMap::new(),
            &[],
            &siblings(),
        );
        assert_eq!(
            link,
            Link::Teammate {
                team_name: "ghost".to_string(),
                team_dir: None,
            }
        );
    }

    #[test]
    fn an_agent_is_never_its_own_parent() {
        let tool_uses = HashMap::from([("toolu_x".to_string(), sub("a"))]);
        let link = resolve_link(
            Some(&meta(Some("toolu_x"), None)),
            &sub("a"),
            &tool_uses,
            &[],
            &siblings(),
        );
        assert_eq!(
            link,
            Link::Unresolved(Unresolved::ToolUseNotFound {
                tool_use_id: "toolu_x".to_string(),
            })
        );
    }

    #[test]
    fn effort_keeps_scalars_as_text_and_drops_structures() {
        assert_eq!(scalar_text(&Value::from("high")), Some("high".to_string()));
        assert_eq!(scalar_text(&Value::from(3)), Some("3".to_string()));
        assert_eq!(scalar_text(&serde_json::json!({"level": 3})), None);
        assert_eq!(scalar_text(&Value::Null), None);
    }

    #[test]
    fn meta_finished_reads_a_boolean_and_stays_none_while_running() {
        let root =
            std::env::temp_dir().join(format!("strate-core-finished-{}", std::process::id()));
        let subagents = root.join("projects/-work-x/s1/subagents");
        fs::create_dir_all(&subagents).expect("temp dirs");
        for (id, meta) in [
            ("a1", r#"{"agentType":"x","finished":true}"#),
            ("a2", r#"{"agentType":"x"}"#),
            ("a3", r#"{"agentType":"x","finished":"yes"}"#),
        ] {
            fs::write(
                subagents.join(format!("agent-{id}.jsonl")),
                "{\"type\":\"user\"}\n",
            )
            .expect("write");
            fs::write(subagents.join(format!("agent-{id}.meta.json")), meta).expect("write");
        }

        let g = discover(&root).expect("readable root");
        fs::remove_dir_all(&root).expect("cleanup");

        let finished = |id: &str| {
            g.subagents
                .iter()
                .find(|s| s.agent_id == id)
                .and_then(|s| s.meta.as_ref())
                .and_then(|m| m.finished)
        };
        assert_eq!(finished("a1"), Some(true));
        assert_eq!(finished("a2"), None, "absent while the subagent runs");
        assert_eq!(finished("a3"), None, "only a boolean counts");
    }

    /// Every fixture home under `tests/fixtures`.
    fn fixture_homes() -> Vec<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut homes: Vec<PathBuf> = fs::read_dir(dir)
            .expect("fixtures")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.join("projects").is_dir())
            .collect();
        homes.sort();
        homes
    }

    /// Blank lines count toward the numbers; a line that does not parse
    /// (malformed, non-UTF-8, or a final one cut short) is skipped and
    /// numbered; CRLF endings parse.
    #[test]
    fn a_read_numbers_the_lines_that_do_not_parse_and_carries_on() {
        let dir = crate::tail::test_dir("scan-lines");
        let path = dir.join("s.jsonl");
        let mut bytes = b"{\"cwd\":\"/work/a\"}\r\n\n{\"a\": \n{\"a\":\"\xff\xfe\"}\n".to_vec();
        bytes.extend(dispatching("toolu_1").as_bytes());
        let cut = b"{\"a\":2,\"b\":\"tru";
        bytes.extend(cut);
        fs::write(&path, &bytes).expect("write");
        let mut t = Transcript::default();
        let mut read = 0;
        t.read_from(&path, &mut read).expect("read");
        fs::remove_dir_all(dir).expect("cleanup");

        assert_eq!(read, bytes.len() as u64);
        assert_eq!((t.lines, t.offset), (5, (bytes.len() - cut.len()) as u64));
        assert_eq!(t.complete.bad, [3, 4]);
        assert_eq!(t.partial.bad, [6]);
        assert_eq!(t.complete.tool_uses, ["toolu_1"]);
        assert_eq!(t.first(|r| &r.cwd).as_deref(), Some("/work/a"));
    }

    fn len(path: &Path) -> u64 {
        fs::metadata(path).expect("metadata").len()
    }

    #[test]
    fn a_pass_with_nothing_changed_reads_no_transcript_bytes_and_links_the_same() {
        let homes = fixture_homes();
        assert!(homes.len() > 5, "{homes:?}");
        for root in homes {
            let mut scanner = Scanner::new();
            let full = scanner.discover(&root).expect("fixture root");
            let every: u64 = transcript_paths(&root).iter().map(|p| len(p)).sum();
            assert_eq!(
                scanner.bytes_read(),
                every,
                "{root:?}: a first pass reads all"
            );
            let again = scanner.discover(&root).expect("fixture root");
            assert_eq!(scanner.bytes_read(), every, "{root:?}: nothing changed");
            assert_eq!(again, full, "{root:?}");
        }
    }

    fn write(path: &Path, text: &str) -> u64 {
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, text).expect("write");
        text.len() as u64
    }

    fn append(path: &Path, text: &str) -> u64 {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open");
        file.write_all(text.as_bytes()).expect("append");
        text.len() as u64
    }

    /// An assistant record holding one `tool_use` with `id`.
    fn dispatching(id: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\"}}]}}}}\n"
        )
    }

    fn link_of(g: &Graph, agent_id: &str) -> Link {
        let sub = g.subagents.iter().find(|s| s.agent_id == agent_id);
        sub.expect("subagent").link.clone()
    }

    fn session_ref() -> AgentRef {
        AgentRef::Session {
            session_id: "s1".to_string(),
        }
    }

    /// A pass of `scanner`, checked against a full scan of the same files,
    /// with the transcript bytes it read.
    fn pass(scanner: &mut Scanner, root: &Path) -> (Graph, u64) {
        let before = scanner.bytes_read();
        let g = scanner.discover(root).expect("readable root");
        assert_eq!(g, Scanner::new().discover(root).expect("readable root"));
        (g, scanner.bytes_read() - before)
    }

    #[test]
    fn a_pass_reads_only_the_bytes_appended_and_the_transcripts_added_since_the_last() {
        let root = crate::tail::test_dir("scan-grows");
        let session = root.join("projects/-work-x/s1.jsonl");
        let subagents = root.join("projects/-work-x/s1/subagents");
        write(&session, "{\"type\":\"user\",\"cwd\":\"/work/x\"}\n");
        write(&subagents.join("agent-a1.jsonl"), "{\"type\":\"user\"}\n");
        let meta = r#"{"agentType":"x","toolUseId":"toolu_1"}"#;
        write(&subagents.join("agent-a1.meta.json"), meta);
        let mut scanner = Scanner::new();
        let (g, _) = pass(&mut scanner, &root);
        assert!(matches!(
            link_of(&g, "a1"),
            Link::Unresolved(Unresolved::ToolUseNotFound { .. })
        ));

        let grown = append(&session, &dispatching("toolu_1"));
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, grown, "only the appended bytes");
        assert_eq!(
            link_of(&g, "a1"),
            Link::Dispatch {
                parent: session_ref(),
                tool_use_id: "toolu_1".to_string(),
            }
        );
        assert_eq!(g.sessions[0].cwd.as_deref(), Some("/work/x"));

        // a2 (spawned by a1) dispatches a3; both appear at once.
        let added = write(&subagents.join("agent-a2.jsonl"), &dispatching("toolu_2"))
            + write(&subagents.join("agent-a3.jsonl"), "{\"type\":\"user\"}\n");
        let meta = r#"{"agentType":"x","parentAgentId":"a1"}"#;
        write(&subagents.join("agent-a2.meta.json"), meta);
        let meta = r#"{"agentType":"x","toolUseId":"toolu_2"}"#;
        write(&subagents.join("agent-a3.meta.json"), meta);
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, added, "only the new transcripts");
        let a2 = AgentRef::Subagent {
            session_id: "s1".to_string(),
            agent_id: "a2".to_string(),
        };
        assert_eq!(
            link_of(&g, "a3"),
            Link::Dispatch {
                parent: a2,
                tool_use_id: "toolu_2".to_string(),
            }
        );
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_line_without_its_newline_is_read_again_once_it_ends() {
        let root = crate::tail::test_dir("scan-partial");
        let session = root.join("projects/-work-x/s1.jsonl");
        let head = "{\"type\":\"user\"}\n";
        let record = dispatching("toolu_1");
        let (start, rest) = record.split_at(30);
        let subagents = root.join("projects/-work-x/s1/subagents");
        write(&subagents.join("agent-a1.jsonl"), "{\"type\":\"user\"}\n");
        let meta = r#"{"agentType":"x","toolUseId":"toolu_1"}"#;
        write(&subagents.join("agent-a1.meta.json"), meta);
        let partial = write(&session, &format!("{head}{start}"));
        let mut scanner = Scanner::new();
        let (g, _) = pass(&mut scanner, &root);
        let warning = g.warnings.iter().find(|w| w.path == session);
        assert_eq!(
            warning.map(|w| &w.kind),
            Some(&WarningKind::MalformedLines(vec![2]))
        );

        append(&session, rest);
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, partial - head.len() as u64 + rest.len() as u64);
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);
        assert!(matches!(link_of(&g, "a1"), Link::Dispatch { .. }));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_transcript_that_shrinks_or_is_replaced_is_read_whole() {
        let root = crate::tail::test_dir("scan-rewritten");
        let session = root.join("projects/-work-x/s1.jsonl");
        let subagents = root.join("projects/-work-x/s1/subagents");
        write(&subagents.join("agent-a1.jsonl"), "{\"type\":\"user\"}\n");
        let meta = r#"{"agentType":"x","toolUseId":"toolu_1"}"#;
        write(&subagents.join("agent-a1.meta.json"), meta);
        write(
            &session,
            &format!("{{\"type\":\"user\"}}\n{}", dispatching("toolu_1")),
        );
        let mut scanner = Scanner::new();
        let (g, _) = pass(&mut scanner, &root);
        assert!(matches!(link_of(&g, "a1"), Link::Dispatch { .. }));

        let shorter = write(&session, "{\"type\":\"user\"}\n");
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, shorter, "truncated in place: read from the start");
        assert!(matches!(
            link_of(&g, "a1"),
            Link::Unresolved(Unresolved::ToolUseNotFound { .. })
        ));

        // Longer than before, so only the file's identity tells it apart.
        let replacement = root.join("projects/-work-x/s1.jsonl.tmp");
        let text = format!("{}{}", dispatching("toolu_1"), dispatching("toolu_9"));
        let longer = write(&replacement, &text);
        fs::rename(&replacement, &session).expect("replace");
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, longer, "replaced: read from the start");
        assert!(matches!(link_of(&g, "a1"), Link::Dispatch { .. }));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_transcript_rewritten_to_the_same_size_with_a_newer_mtime_is_read_whole() {
        let root = crate::tail::test_dir("scan-same-size");
        let session = root.join("projects/-work-x/s1.jsonl");
        let subagents = root.join("projects/-work-x/s1/subagents");
        write(&subagents.join("agent-a1.jsonl"), "{\"type\":\"user\"}\n");
        let meta = r#"{"agentType":"x","toolUseId":"toolu_1"}"#;
        write(&subagents.join("agent-a1.meta.json"), meta);
        write(&session, &dispatching("toolu_1"));
        let mut scanner = Scanner::new();
        let (g, _) = pass(&mut scanner, &root);
        assert!(matches!(link_of(&g, "a1"), Link::Dispatch { .. }));

        let size = write(&session, &dispatching("toolu_2"));
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&session)
            .expect("open");
        let newer = fs::metadata(&session)
            .expect("metadata")
            .modified()
            .expect("mtime")
            + std::time::Duration::from_secs(5);
        file.set_modified(newer).expect("set mtime");
        drop(file);
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, size, "same size, newer mtime: read from the start");
        assert!(matches!(
            link_of(&g, "a1"),
            Link::Unresolved(Unresolved::ToolUseNotFound { .. })
        ));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn a_deleted_transcript_is_forgotten_and_restored_when_it_reappears() {
        let root = crate::tail::test_dir("scan-deleted");
        let session = root.join("projects/-work-x/s1.jsonl");
        let subagents = root.join("projects/-work-x/s1/subagents");
        write(&subagents.join("agent-a1.jsonl"), "{\"type\":\"user\"}\n");
        let meta = r#"{"agentType":"x","toolUseId":"toolu_1"}"#;
        write(&subagents.join("agent-a1.meta.json"), meta);
        let text = dispatching("toolu_1");
        write(&session, &text);
        let mut scanner = Scanner::new();
        let (g, _) = pass(&mut scanner, &root);
        assert!(matches!(link_of(&g, "a1"), Link::Dispatch { .. }));

        fs::remove_file(&session).expect("delete");
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, 0);
        assert!(g.sessions.is_empty());
        assert!(matches!(
            link_of(&g, "a1"),
            Link::Unresolved(Unresolved::ToolUseNotFound { .. })
        ));
        assert!(!scanner.last.contains_key(&session), "entry dropped");

        let back = write(&session, &text);
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, back, "read whole again");
        assert!(matches!(link_of(&g, "a1"), Link::Dispatch { .. }));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    /// `finished` is as fresh as a full scan's: every pass reads the metas.
    #[test]
    fn a_meta_rewritten_between_passes_is_read_again() {
        let root = crate::tail::test_dir("scan-meta");
        let subagents = root.join("projects/-work-x/s1/subagents");
        write(&subagents.join("agent-a1.jsonl"), "{\"type\":\"user\"}\n");
        let meta = subagents.join("agent-a1.meta.json");
        write(&meta, r#"{"agentType":"x"}"#);
        let mut scanner = Scanner::new();
        let finished = |g: &Graph| g.subagents[0].meta.as_ref().and_then(|m| m.finished);
        let (g, _) = pass(&mut scanner, &root);
        assert_eq!(finished(&g), None);

        write(&meta, r#"{"agentType":"x","finished":true}"#);
        let (g, read) = pass(&mut scanner, &root);
        assert_eq!(read, 0, "no transcript changed");
        assert_eq!(finished(&g), Some(true));
        fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn malformed_meta_and_team_config_warn_without_aborting() {
        let root = std::env::temp_dir().join(format!("strate-core-scan-{}", std::process::id()));
        let subagents = root.join("projects/-work-x/s1/subagents");
        fs::create_dir_all(&subagents).expect("temp dirs");
        fs::create_dir_all(root.join("teams/session-00000000")).expect("temp dirs");
        fs::write(subagents.join("agent-a0.jsonl"), "{\"type\":\"user\"}\n").expect("write");
        fs::write(subagents.join("agent-a0.meta.json"), "[1,2").expect("write");
        fs::write(
            root.join("teams/session-00000000/config.json"),
            "\"not an object\"",
        )
        .expect("write");

        let g = discover(&root).expect("readable root");
        fs::remove_dir_all(&root).expect("cleanup");

        assert!(g.teams.is_empty());
        assert!(g.sessions.is_empty(), "no s1.jsonl, so no session node");
        assert_eq!(g.subagents.len(), 1);
        assert_eq!(g.subagents[0].meta, None);
        assert_eq!(g.subagents[0].link, Link::Unresolved(Unresolved::NoMeta));
        let malformed: Vec<&Warning> = g
            .warnings
            .iter()
            .filter(|w| w.kind == WarningKind::MalformedJson)
            .collect();
        assert_eq!(malformed.len(), 2, "{:?}", g.warnings);
    }
}
