use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::jsonl::for_each_record;
use super::{
    AgentRef, Graph, Link, Project, SessionNode, SubagentMeta, SubagentNode, TeamNode, Unresolved,
    Warning, WarningKind,
};

pub(super) fn discover(root: &Path) -> io::Result<Graph> {
    fs::read_dir(root)?;
    let mut g = Graph::default();
    g.teams = read_teams(&root.join("teams"), &mut g.warnings);
    for path in sorted_entries(&root.join("projects"), &mut g.warnings) {
        if path.is_dir() {
            let dir_name = name_of(&path);
            scan_project(&path, &dir_name, &mut g);
            g.projects.push(Project { dir_name, path });
        }
    }
    Ok(g)
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

fn scan_project(project: &Path, dir_name: &str, g: &mut Graph) {
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
        scan_session(project, dir_name, &session_id, g);
    }
}

fn scan_session(project: &Path, dir_name: &str, session_id: &str, g: &mut Graph) {
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
        read_transcript(&transcript, &mut g.warnings, |record| {
            fill_first(&mut node.cwd, &record, "cwd");
            fill_first(&mut node.git_branch, &record, "gitBranch");
            fill_first(&mut node.version, &record, "version");
            collect_tool_uses(&record, &owner, &mut tool_uses);
        });
        g.sessions.push(node);
    }

    let mut pending = Vec::new();
    for path in sorted_entries(&project.join(session_id).join("subagents"), &mut g.warnings) {
        let file = name_of(&path);
        let Some(agent_id) = file
            .strip_prefix("agent-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let owner = AgentRef::Subagent {
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
        };
        read_transcript(&path, &mut g.warnings, |record| {
            collect_tool_uses(&record, &owner, &mut tool_uses);
        });
        let meta = read_meta(&path, agent_id, &mut g.warnings);
        pending.push((agent_id.to_string(), owner, path, meta));
    }

    // Links resolve only after every transcript in the session is read, so
    // a nested subagent finds a parent subagent listed after it.
    for (agent_id, owner, path, meta) in pending {
        let link = resolve_link(meta.as_ref(), &owner, &tool_uses, &g.teams);
        g.subagents.push(SubagentNode {
            session_id: session_id.to_string(),
            agent_id,
            path,
            meta,
            link,
        });
    }
}

fn read_transcript(path: &Path, warnings: &mut Vec<Warning>, on_record: impl FnMut(Value)) {
    let result = File::open(path).and_then(|f| for_each_record(BufReader::new(f), on_record));
    match result {
        Ok(bad) if bad.is_empty() => {}
        Ok(bad) => warn(warnings, path, WarningKind::MalformedLines(bad)),
        Err(e) => warn(warnings, path, WarningKind::Unreadable(e.to_string())),
    }
}

fn fill_first(slot: &mut Option<String>, record: &Value, key: &str) {
    if slot.is_none() {
        *slot = record.get(key).and_then(Value::as_str).map(str::to_string);
    }
}

fn collect_tool_uses(record: &Value, owner: &AgentRef, out: &mut HashMap<String, AgentRef>) {
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(blocks) = record.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_use")
            && let Some(id) = block.get("id").and_then(Value::as_str)
        {
            out.entry(id.to_string()).or_insert_with(|| owner.clone());
        }
    }
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
        description: text("description"),
        model: text("model"),
        effort: map.get("effort").and_then(scalar_text),
        spawn_depth: map.get("spawnDepth").and_then(Value::as_u64),
        tool_use_id: text("toolUseId"),
        name: text("name"),
        team_name: text("teamName"),
    })
}

/// `effort`'s type is unverified: keep strings as-is and render other
/// scalars as text rather than dropping them.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn resolve_link(
    meta: Option<&SubagentMeta>,
    owner: &AgentRef,
    tool_uses: &HashMap<String, AgentRef>,
    teams: &[TeamNode],
) -> Link {
    let Some(meta) = meta else {
        return Link::Unresolved(Unresolved::NoMeta);
    };
    if let Some(tool_use_id) = &meta.tool_use_id {
        return match tool_uses.get(tool_use_id) {
            Some(parent) if parent != owner => Link::Dispatch {
                parent: parent.clone(),
                tool_use_id: tool_use_id.clone(),
            },
            _ => Link::Unresolved(Unresolved::ToolUseNotFound {
                tool_use_id: tool_use_id.clone(),
            }),
        };
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

    #[test]
    fn meta_without_tool_use_id_or_team_is_unresolved() {
        let link = resolve_link(Some(&meta(None, None)), &sub("a"), &HashMap::new(), &[]);
        assert_eq!(link, Link::Unresolved(Unresolved::NoToolUseId));
    }

    #[test]
    fn teammate_of_unknown_team_keeps_the_name_without_a_dir() {
        let link = resolve_link(
            Some(&meta(None, Some("ghost"))),
            &sub("a"),
            &HashMap::new(),
            &[],
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
