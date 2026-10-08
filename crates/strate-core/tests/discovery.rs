//! Integration tests over the synthetic fixture in `tests/fixtures/claude-home`.
//! The fixture is hand-built from field names only (the repo is public).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use strate_core::discovery::{
    AgentRef, Graph, Link, SubagentNode, Unresolved, WarningKind, discover,
};

const SA: &str = "5e55a0a0-0000-4000-8000-00000000000a";
const SB: &str = "5e55b0b0-0000-4000-8000-00000000000b";
const LONG_DIR: &str = "-work--worktrees-demo-repo-7-lorem-ipsum-dolor-sit-x7k2q9";

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-home")
}

fn graph() -> Graph {
    discover(&fixture_root()).expect("fixture root is readable")
}

fn subagent<'g>(g: &'g Graph, agent_id: &str) -> &'g SubagentNode {
    g.subagents
        .iter()
        .find(|s| s.agent_id == agent_id)
        .unwrap_or_else(|| panic!("subagent {agent_id} not discovered"))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("readable fixture dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 name")
}

/// Independent oracle: which transcript in the session holds a `tool_use`
/// line with this id? Plain text search, deliberately not the crate's parser.
fn expected_parent(session_dir: &Path, self_path: &Path, tool_use_id: &str) -> Option<AgentRef> {
    let session_id = file_name(session_dir).to_string();
    let needle = format!("\"type\":\"tool_use\",\"id\":\"{tool_use_id}\"");
    let mut candidates = vec![session_dir.with_extension("jsonl")];
    if let Ok(entries) = fs::read_dir(session_dir.join("subagents")) {
        let mut subs: Vec<PathBuf> = entries.map(|e| e.expect("entry").path()).collect();
        subs.sort();
        candidates.extend(
            subs.into_iter()
                .filter(|p| file_name(p).ends_with(".jsonl")),
        );
    }
    candidates
        .into_iter()
        .filter(|p| p != self_path)
        .find(|p| fs::read_to_string(p).is_ok_and(|text| text.contains(&needle)))
        .map(|p| {
            let name = file_name(&p);
            match name.strip_prefix("agent-") {
                Some(rest) => AgentRef::Subagent {
                    session_id: session_id.clone(),
                    agent_id: rest.trim_end_matches(".jsonl").to_string(),
                },
                None => AgentRef::Session {
                    session_id: session_id.clone(),
                },
            }
        })
}

#[test]
fn done_when_graph_matches_every_meta_json_exactly() {
    let root = fixture_root();
    let g = graph();
    let mut files = Vec::new();
    walk(&root.join("projects"), &mut files);

    let metas: Vec<&PathBuf> = files
        .iter()
        .filter(|p| file_name(p).ends_with(".meta.json"))
        .collect();
    let orphan_jsonls = files
        .iter()
        .filter(|p| {
            let name = file_name(p);
            name.starts_with("agent-")
                && name.ends_with(".jsonl")
                && !p
                    .with_file_name(name.replace(".jsonl", ".meta.json"))
                    .exists()
        })
        .count();
    assert!(
        metas.len() >= 6,
        "fixture should carry the meta key-set variety"
    );

    let with_meta: Vec<&SubagentNode> = g.subagents.iter().filter(|s| s.meta.is_some()).collect();
    assert_eq!(with_meta.len(), metas.len(), "one node per .meta.json");
    assert_eq!(g.subagents.len(), metas.len() + orphan_jsonls);

    for meta_path in metas {
        let raw: Value =
            serde_json::from_str(&fs::read_to_string(meta_path).expect("meta")).expect("json");
        let agent_id = file_name(meta_path)
            .trim_start_matches("agent-")
            .trim_end_matches(".meta.json");
        let session_dir = meta_path
            .parent()
            .and_then(Path::parent)
            .expect("session dir");
        let node = subagent(&g, agent_id);
        let meta = node.meta.as_ref().expect("meta parsed");

        assert_eq!(
            node.session_id,
            file_name(session_dir),
            "{agent_id} session"
        );
        assert_eq!(
            meta.agent_type.as_deref(),
            raw["agentType"].as_str(),
            "{agent_id} agentType"
        );
        assert_eq!(
            meta.model.as_deref(),
            raw["model"].as_str(),
            "{agent_id} model"
        );

        let self_path = meta_path.with_file_name(format!("agent-{agent_id}.jsonl"));
        match (raw["toolUseId"].as_str(), raw["teamName"].as_str()) {
            (Some(tool_use_id), _) => {
                let expected = match expected_parent(session_dir, &self_path, tool_use_id) {
                    Some(parent) => Link::Dispatch {
                        parent,
                        tool_use_id: tool_use_id.to_string(),
                    },
                    None => Link::Unresolved(Unresolved::ToolUseNotFound {
                        tool_use_id: tool_use_id.to_string(),
                    }),
                };
                assert_eq!(node.link, expected, "{agent_id} parent");
            }
            (None, Some(team_name)) => {
                assert!(
                    matches!(&node.link, Link::Teammate { team_name: t, .. } if t == team_name),
                    "{agent_id} teammate link, got {:?}",
                    node.link
                );
            }
            (None, None) => assert_eq!(node.link, Link::Unresolved(Unresolved::NoToolUseId)),
        }
    }
}

#[test]
fn scans_every_project_dir_including_lossy_names() {
    let g = graph();
    let mut dirs: Vec<&str> = g.projects.iter().map(|p| p.dir_name.as_str()).collect();
    dirs.sort_unstable();
    assert_eq!(dirs, vec![LONG_DIR, "-work-demo-repo"]);
}

#[test]
fn session_nodes_carry_cwd_branch_and_version_from_records() {
    let g = graph();
    assert_eq!(g.sessions.len(), 2);

    let a = g
        .sessions
        .iter()
        .find(|s| s.session_id == SA)
        .expect("session A");
    assert_eq!(a.project_dir, "-work-demo-repo");
    // The first record is metadata with no cwd; cwd comes from the first record carrying one.
    assert_eq!(a.cwd.as_deref(), Some("/work/demo-repo"));
    assert_eq!(a.git_branch.as_deref(), Some("main"));
    assert_eq!(a.version.as_deref(), Some("2.1.294"));

    let b = g
        .sessions
        .iter()
        .find(|s| s.session_id == SB)
        .expect("session B");
    assert_eq!(b.project_dir, LONG_DIR);
    assert_eq!(b.cwd.as_deref(), Some("/work/.worktrees/demo-repo-7"));
    assert_eq!(b.git_branch.as_deref(), Some("feat/7-lorem"));
    assert_eq!(b.version.as_deref(), Some("2.1.263"));
}

#[test]
fn subagent_meta_fields_are_read_and_absent_ones_stay_none() {
    let g = graph();
    let implementer = subagent(&g, "a1000000000000001")
        .meta
        .as_ref()
        .expect("meta");
    assert_eq!(implementer.agent_type.as_deref(), Some("implementer"));
    assert_eq!(implementer.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(implementer.effort.as_deref(), Some("high"));
    assert_eq!(implementer.spawn_depth, Some(1));
    assert_eq!(implementer.description.as_deref(), Some("Lorem ipsum task"));
    assert_eq!(
        implementer.tool_use_id.as_deref(),
        Some("toolu_01AAAAAAAAAAAAAAAAAAAAAA")
    );

    let recon = subagent(&g, "a1000000000000002")
        .meta
        .as_ref()
        .expect("meta");
    assert_eq!(recon.agent_type.as_deref(), Some("recon"));
    assert_eq!(recon.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(recon.effort, None);

    let no_model = subagent(&g, "a1000000000000003")
        .meta
        .as_ref()
        .expect("meta");
    assert_eq!(no_model.model, None);
    assert_eq!(no_model.spawn_depth, Some(2));
}

#[test]
fn nested_subagent_resolves_to_its_subagent_parent() {
    let g = graph();
    assert_eq!(
        subagent(&g, "a1000000000000003").link,
        Link::Dispatch {
            parent: AgentRef::Subagent {
                session_id: SA.to_string(),
                agent_id: "a1000000000000001".to_string(),
            },
            tool_use_id: "toolu_01CCCCCCCCCCCCCCCCCCCCCC".to_string(),
        }
    );
}

#[test]
fn top_level_subagents_resolve_to_their_session() {
    let g = graph();
    let session_a = AgentRef::Session {
        session_id: SA.to_string(),
    };
    for id in ["a1000000000000001", "a1000000000000002"] {
        assert!(
            matches!(&subagent(&g, id).link, Link::Dispatch { parent, .. } if *parent == session_a),
            "{id}"
        );
    }
    assert!(matches!(
        &subagent(&g, "a2000000000000001").link,
        Link::Dispatch { parent: AgentRef::Session { session_id }, .. } if session_id == SB
    ));
}

#[test]
fn orphan_jsonl_without_meta_is_kept_and_flagged() {
    let g = graph();
    let orphan = subagent(&g, "a1000000000000004");
    assert_eq!(orphan.meta, None);
    assert_eq!(orphan.link, Link::Unresolved(Unresolved::NoMeta));
    assert!(
        g.warnings.iter().any(|w| w.kind == WarningKind::MissingMeta
            && w.path.ends_with("agent-a1000000000000004.jsonl"))
    );
}

#[test]
fn unresolvable_tool_use_id_is_recorded_not_attached_to_session() {
    let g = graph();
    assert_eq!(
        subagent(&g, "a1000000000000005").link,
        Link::Unresolved(Unresolved::ToolUseNotFound {
            tool_use_id: "toolu_01ZZZZZZZZZZZZZZZZZZZZZZ".to_string(),
        })
    );
}

#[test]
fn teammate_links_to_its_team_not_a_dispatch_edge() {
    let g = graph();
    let mate = subagent(&g, "a1000000000000006");
    assert_eq!(
        mate.link,
        Link::Teammate {
            team_name: "demo-team".to_string(),
            team_dir: Some("session-0a1b2c3d".to_string()),
        }
    );
    let meta = mate.meta.as_ref().expect("meta");
    assert_eq!(meta.name.as_deref(), Some("demo-reviewer"));
    assert_eq!(meta.spawn_depth, Some(0));
    assert_eq!(meta.tool_use_id, None);
}

#[test]
fn team_config_becomes_a_team_node() {
    let g = graph();
    assert_eq!(g.teams.len(), 1);
    let team = &g.teams[0];
    assert_eq!(team.dir_name, "session-0a1b2c3d");
    assert_eq!(team.name.as_deref(), Some("demo-team"));
    assert_eq!(team.lead_agent_id.as_deref(), Some("lead-0001"));
    assert_eq!(team.lead_session_id.as_deref(), Some(SA));
    assert_eq!(
        team.created_at.as_ref().and_then(|n| n.as_u64()),
        Some(1_790_000_000_000)
    );
    assert_eq!(team.members.len(), 2);
    assert_eq!(team.members[1], Value::from("opaque-member"));
}

#[test]
fn malformed_and_truncated_lines_become_warnings() {
    let g = graph();
    let session_file = format!("{SA}.jsonl");
    let warning = g
        .warnings
        .iter()
        .find(|w| w.path.ends_with(&session_file))
        .expect("warning for session A transcript");
    assert_eq!(warning.kind, WarningKind::MalformedLines(vec![9, 12]));
}

#[test]
fn replayed_tool_use_does_not_duplicate_dispatch() {
    let g = graph();
    let dispatched = g
        .subagents
        .iter()
        .filter(|s| matches!(&s.link, Link::Dispatch { tool_use_id, .. } if tool_use_id == "toolu_01AAAAAAAAAAAAAAAAAAAAAA"))
        .count();
    assert_eq!(dispatched, 1);
}

#[test]
fn credentials_and_live_session_records_are_never_read() {
    // Both decoys hold garbage; any attempt to parse them would surface a warning.
    let root = fixture_root();
    assert!(root.join(".credentials.json").exists());
    assert!(root.join("sessions/123.json").exists());
    let g = graph();
    for w in &g.warnings {
        assert!(!w.path.starts_with(root.join("sessions")), "{w:?}");
        assert!(!w.path.ends_with(".credentials.json"), "{w:?}");
    }
    assert!(g.sessions.iter().all(|s| s.session_id != "123"));
}

#[test]
fn tool_results_dir_is_not_a_subagent_source() {
    let g = graph();
    assert_eq!(g.subagents.iter().filter(|s| s.session_id == SA).count(), 6);
}

#[test]
fn missing_root_is_an_error() {
    let missing = fixture_root().join("does-not-exist");
    assert!(discover(&missing).is_err());
}

#[test]
fn root_without_projects_or_teams_is_an_empty_graph() {
    let empty = fixture_root().join("sessions");
    let g = discover(&empty).expect("readable dir");
    assert_eq!(g, Graph::default());
}
