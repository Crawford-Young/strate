//! The Audit rollups (#26): a tree over `audit_cells`, plus a headline, read
//! on a reader connection.
//!
//! Levels: Repo > Epic > Issue > PR > Session > Agent. A Session node is one
//! `/rename` window. Its name (`<repo>-<issue>[-sN]`) places it under its
//! repo and issue. When the GitHub cache holds that issue, it also goes
//! under the epic the issue is a sub-issue of, and under a PR: the issue's
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

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::Result;
use crate::names::{ParsedName, parse_name};
use crate::time::parse_timestamp;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Root,
    /// A value of the group-by: a day, `<repo>#<pr>`, an agent type or a
    /// model (`""`: none).
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
    /// Some of `agent_ms` is a background subagent's, whose timing is
    /// approximate until #42.
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

/// The closed issues and merged PRs with work in the filter, over the
/// hierarchy whatever the group-by.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Headline {
    pub issues_closed: i64,
    pub median_elapsed_ms: Option<i64>,
    /// Their $ over their count.
    pub usd_per_closed_issue: Option<f64>,
    pub usd_per_merged_pr: Option<f64>,
    /// Cloud sessions are not in the store (#29), so their spend is missing.
    pub cloud_uncounted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Audit {
    pub root: AuditNode,
    pub headline: Headline,
}

/// The rollup tree and headline for `filter`. Fails on a day that is not
/// `YYYY-MM-DD`.
pub fn audit(conn: &Connection, filter: &AuditFilter) -> Result<Audit> {
    let gh = Gh::load(conn)?;
    let cells = Cells::load(conn, &gh, filter)?;
    let hierarchy = |atom: &Atom| path(atom, &cells.windows.list[atom.window], &gh, &cells.agents);
    let mut tally = Tally::default();
    let mut tree = Node::root();
    for atom in &cells.atoms {
        tree.insert(hierarchy(atom), &atom.share);
    }
    let mut root = tree.finish(&mut tally).0;
    if filter.group_by != GroupBy::Hierarchy {
        let mut tree = Node::root();
        for atom in &cells.atoms {
            let window = &cells.windows.list[atom.window];
            let mut steps = vec![group(filter.group_by, atom, window, &cells.agents)];
            steps.extend(hierarchy(atom));
            tree.insert(steps, &atom.share);
        }
        root = tree.finish(&mut Tally::default()).0;
    }
    Ok(Audit {
        root,
        headline: tally.headline(),
    })
}

/// The cells in a filter, with the windows and subagents they belong to.
struct Cells {
    atoms: Vec<Atom>,
    windows: Windows,
    agents: HashMap<(String, String), Agent>,
}

impl Cells {
    /// Cells come in day order, the orchestrator's first, so a window or
    /// subagent counts on its first cell in the filter.
    fn load(conn: &Connection, gh: &Gh, filter: &AuditFilter) -> Result<Self> {
        let (from, to) = (day(&filter.from)?, day(&filter.to)?);
        let mut out = Self {
            atoms: Vec::new(),
            windows: Windows::default(),
            agents: HashMap::new(),
        };
        let mut seen_windows: HashSet<usize> = HashSet::new();
        let mut seen_agents: HashSet<(String, String)> = HashSet::new();
        let mut stmt = conn.prepare_cached(
            "SELECT session_id, agent_id, segment_id, day, model, wall_ms, active_ms, wait_ms,
                 cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
                 cache_write_tokens
             FROM audit_cells WHERE day BETWEEN ?1 AND ?2
             ORDER BY day, agent_id IS NOT NULL, agent_id, session_id, segment_id, model, id",
        )?;
        let mut rows = stmt.query([from, to])?;
        while let Some(r) = rows.next()? {
            let session_id: String = r.get(0)?;
            let agent_id: Option<String> = r.get(1)?;
            let window = out.windows.get(conn, gh, &session_id, r.get(2)?)?;
            let w = &out.windows.list[window];
            let wanted = |repo: &String| filter.repos.contains(repo);
            if !filter.repos.is_empty() && !w.repo.as_ref().is_some_and(wanted) {
                continue;
            }
            let first_in_window = seen_windows.insert(window);
            let mut share = Share {
                cost_usd: r.get(8)?,
                unpriced: r.get(9)?,
                tokens: [r.get(10)?, r.get(11)?, r.get(12)?, r.get(13)?],
                own: [r.get(6)?, r.get(7)?],
                sessions: first_in_window.into(),
                rotations: (first_in_window && w.rotation).into(),
                ..Share::default()
            };
            match &agent_id {
                None => share.line = share.own,
                Some(agent_id) => {
                    let key = (session_id.clone(), agent_id.clone());
                    let agent = match out.agents.entry(key.clone()) {
                        Entry::Occupied(found) => found.into_mut(),
                        Entry::Vacant(slot) => {
                            slot.insert(Agent::load(conn, &session_id, agent_id)?)
                        }
                    };
                    share.agent_ms = r.get(5)?;
                    share.approximate = agent.background;
                    share.agents = seen_agents.insert(key).into();
                }
            }
            out.atoms.push(Atom {
                session_id,
                agent_id,
                window,
                day: r.get(3)?,
                model: r.get(4)?,
                share,
            });
        }
        Ok(out)
    }
}

/// `s` when it is a `YYYY-MM-DD` day.
fn day(s: &str) -> Result<&str> {
    match s.parse::<jiff::civil::Date>() {
        Ok(d) if d.to_string() == s => Ok(s),
        _ => Err(rusqlite::Error::ToSqlConversionFailure(
            format!("not a YYYY-MM-DD day: {s:?}").into(),
        )),
    }
}

/// One cell in the filter: where it sits and what it adds.
struct Atom {
    session_id: String,
    agent_id: Option<String>,
    window: usize,
    day: String,
    model: Option<String>,
    share: Share,
}

/// What one cell adds to each node above it.
#[derive(Default)]
struct Share {
    cost_usd: Option<f64>,
    unpriced: i64,
    tokens: [i64; 4],
    agent_ms: i64,
    approximate: bool,
    /// Active and waiting on the session line: the orchestrator's cells only.
    line: [i64; 2],
    /// The cell's own active and waiting, for its Agent node.
    own: [i64; 2],
    sessions: i64,
    agents: i64,
    rotations: i64,
}

/// The cached GitHub issues and their closing PRs.
struct Gh {
    issues: HashMap<(String, i64), GhIssue>,
    prs: HashMap<(String, i64), Vec<GhPr>>,
}

struct GhIssue {
    title: String,
    /// `Some` once closed; the time is `None` if GitHub gave none.
    closed: Option<Option<i64>>,
    parent: Option<i64>,
    progress: Option<Progress>,
}

struct GhPr {
    number: i64,
    state: String,
    merged_at: Option<i64>,
    url: String,
}

impl Gh {
    fn load(conn: &Connection) -> Result<Self> {
        let at = |s: Option<String>| s.as_deref().and_then(parse_timestamp);
        let issues = conn
            .prepare_cached(
                "SELECT repo, number, title, state, closed_at, parent, sub_total, sub_completed
                 FROM gh_issues",
            )?
            .query_map([], |r| {
                let state: String = r.get(3)?;
                let closed_at: Option<String> = r.get(4)?;
                let total: i64 = r.get(6)?;
                let completed: i64 = r.get(7)?;
                Ok((
                    (r.get(0)?, r.get(1)?),
                    GhIssue {
                        title: r.get(2)?,
                        closed: (state == "CLOSED").then(|| at(closed_at)),
                        parent: r.get(5)?,
                        progress: (total > 0).then_some(Progress { completed, total }),
                    },
                ))
            })?
            .collect::<Result<_>>()?;
        let mut prs: HashMap<(String, i64), Vec<GhPr>> = HashMap::new();
        let mut stmt = conn.prepare_cached(
            "SELECT repo, issue, number, state, merged_at, url FROM gh_prs
             ORDER BY repo, issue, number",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((
                (r.get(0)?, r.get(1)?),
                GhPr {
                    number: r.get(2)?,
                    state: r.get(3)?,
                    merged_at: at(r.get(4)?),
                    url: r.get(5)?,
                },
            ))
        })? {
            let (key, pr) = row?;
            prs.entry(key).or_default().push(pr);
        }
        Ok(Self { issues, prs })
    }
}

/// Where a window's work belongs.
enum Place {
    Unlinked,
    Linked {
        repo: String,
        issue: i64,
        pr: Option<i64>,
    },
}

/// A `/rename` window (or a session with none).
struct Window {
    key: String,
    label: String,
    /// The repo its name parses to, linked or not.
    repo: Option<String>,
    /// Named `-sN`, any N: a rotation is a `-sN` window, not "windows - 1".
    rotation: bool,
    place: Place,
    /// Its start to the next window's start, or to the session's last
    /// activity.
    span: Option<(i64, i64)>,
}

/// Every window the filter's cells touch, resolved once.
#[derive(Default)]
struct Windows {
    list: Vec<Window>,
    index: HashMap<(String, Option<i64>), usize>,
    sessions: HashMap<String, Session>,
}

impl Windows {
    /// The index in `list` of the window `segment` of `session_id` (`None`:
    /// the session's cells with no window).
    fn get(
        &mut self,
        conn: &Connection,
        gh: &Gh,
        session_id: &str,
        segment: Option<i64>,
    ) -> Result<usize> {
        let key = (session_id.to_string(), segment);
        if let Some(&i) = self.index.get(&key) {
            return Ok(i);
        }
        let session = match self.sessions.entry(session_id.to_string()) {
            Entry::Occupied(found) => found.into_mut(),
            Entry::Vacant(slot) => slot.insert(Session::load(conn, session_id)?),
        };
        let named = Named::load(conn, session_id, segment)?;
        let parsed = if named.fallback {
            None
        } else {
            parse_name(&named.name)
        };
        let end = named.next.or(session.last);
        self.list.push(Window {
            key: match segment {
                Some(id) => id.to_string(),
                None => format!("session:{session_id}"),
            },
            place: place(gh, parsed.as_ref(), &session.links),
            repo: parsed.as_ref().map(|p| p.repo.clone()),
            rotation: parsed.is_some_and(|p| p.rotation.is_some()),
            label: named.name,
            span: named.start.map(|s| (s, end.unwrap_or(s).max(s))),
        });
        self.index.insert(key, self.list.len() - 1);
        Ok(self.list.len() - 1)
    }
}

/// What every window of one session shares.
struct Session {
    /// Its `github_links`, newest first.
    links: Vec<PrLink>,
    /// Its last activity: the latest record its timeline counts.
    last: Option<i64>,
}

/// A PR a session's transcript linked (`github_links`).
struct PrLink {
    url: String,
    number: Option<i64>,
    /// `owner/name`.
    repository: Option<String>,
}

impl Session {
    fn load(conn: &Connection, session_id: &str) -> Result<Self> {
        let links = conn
            .prepare_cached(
                "SELECT pr_url, pr_number, pr_repository FROM github_links
                 WHERE session_id = ?1 ORDER BY id DESC",
            )?
            .query_map([session_id], |r| {
                Ok(PrLink {
                    url: r.get(0)?,
                    number: r.get(1)?,
                    repository: r.get(2)?,
                })
            })?
            .collect::<Result<_>>()?;
        let last: Option<String> = conn
            .prepare_cached(
                "SELECT max(timestamp) FROM events
                 WHERE session_id = ?1 AND timestamp IS NOT NULL
                   AND ifnull(type, '') NOT IN ('custom-title', 'ai-title', 'cost-state')",
            )?
            .query_row([session_id], |r| r.get(0))?;
        Ok(Self {
            links,
            last: last.as_deref().and_then(parse_timestamp),
        })
    }
}

/// A window's name (merge-resolved) and where it starts and the next one
/// starts; with no window, the session's id and start.
struct Named {
    name: String,
    /// A cwd/branch fallback, not a name the user set.
    fallback: bool,
    start: Option<i64>,
    next: Option<i64>,
}

impl Named {
    fn load(conn: &Connection, session_id: &str, segment: Option<i64>) -> Result<Self> {
        let at = |s: Option<String>| s.as_deref().and_then(parse_timestamp);
        let found = match segment {
            Some(id) => conn
                .prepare_cached(
                    "SELECT w.name, w.fallback, g.started_at,
                         (SELECT n.started_at FROM segments AS n
                          WHERE n.session_id = g.session_id AND n.start_offset > g.start_offset
                          ORDER BY n.start_offset LIMIT 1)
                     FROM segments AS g
                     JOIN workstream_roots AS r ON r.id = g.workstream_id
                     JOIN workstreams AS w ON w.id = r.root
                     WHERE g.id = ?1",
                )?
                .query_row([id], |r| {
                    Ok(Self {
                        name: r.get(0)?,
                        fallback: r.get(1)?,
                        start: at(r.get(2)?),
                        next: at(r.get(3)?),
                    })
                })
                .optional()?,
            None => None,
        };
        if let Some(named) = found {
            return Ok(named);
        }
        let start = conn
            .prepare_cached("SELECT started_at FROM sessions WHERE session_id = ?1")?
            .query_row([session_id], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(Self {
            name: session_id.to_string(),
            fallback: true,
            start: at(start),
            next: None,
        })
    }
}

/// Where a window named `parsed` belongs: under its repo and issue, and
/// under the PR the session links (else the issue's only PR) when the
/// GitHub cache holds the issue. Only a name that doesn't parse is Unlinked.
fn place(gh: &Gh, parsed: Option<&ParsedName>, links: &[PrLink]) -> Place {
    let Some(p) = parsed else {
        return Place::Unlinked;
    };
    let key = (p.repo.clone(), p.issue as i64);
    let prs = gh.prs.get(&key).map_or(&[][..], Vec::as_slice);
    let only = match prs {
        [only] => Some(only.number),
        _ => None,
    };
    Place::Linked {
        pr: linked_pr(prs, links, &p.repo).or(only),
        repo: key.0,
        issue: key.1,
    }
}

/// The newest of the session's links that names one of `prs`, by URL or by
/// number in a repository named `repo`.
fn linked_pr(prs: &[GhPr], links: &[PrLink], repo: &str) -> Option<i64> {
    links.iter().find_map(|link| {
        let same_repo = link
            .repository
            .as_deref()
            .and_then(|r| r.split_once('/'))
            .is_some_and(|(_, name)| name == repo);
        prs.iter()
            .find(|pr| pr.url == link.url || (same_repo && link.number == Some(pr.number)))
            .map(|pr| pr.number)
    })
}

/// A subagent as its node shows it.
struct Agent {
    label: String,
    agent_type: Option<String>,
    run_ms: Option<i64>,
    /// Dispatched with `run_in_background: true` on its Agent tool_use, read
    /// at query time from the parent's stored record (no column of its own).
    background: bool,
}

impl Agent {
    fn load(conn: &Connection, session_id: &str, agent_id: &str) -> Result<Self> {
        let found = conn
            .prepare_cached(
                "SELECT coalesce(a.registry_name, a.name, a.description, a.agent_type), a.agent_type,
                     a.run_ms,
                     EXISTS (SELECT 1 FROM edges AS d
                         JOIN agents AS p ON p.id = d.parent
                         JOIN events AS e ON e.session_id = p.session_id
                             AND e.agent_id IS p.agent_id AND e.tool_uses IS NOT NULL
                         JOIN json_each(e.raw, '$.message.content') AS b
                         WHERE d.child = a.id AND d.kind = 'dispatch'
                           AND instr(e.tool_uses, d.tool_use_id) > 0
                           AND CASE WHEN b.type = 'object' THEN
                               json_extract(b.value, '$.id') = d.tool_use_id
                               AND json_extract(b.value, '$.input.run_in_background') IS 1
                           END)
                 FROM agents AS a WHERE a.session_id = ?1 AND a.agent_id = ?2",
            )?
            .query_row(params![session_id, agent_id], |r| {
                Ok(Self {
                    label: r
                        .get::<_, Option<String>>(0)?
                        .unwrap_or_else(|| agent_id.to_string()),
                    agent_type: r.get(1)?,
                    run_ms: r.get(2)?,
                    background: r.get(3)?,
                })
            })
            .optional()?;
        Ok(found.unwrap_or(Self {
            label: agent_id.to_string(),
            agent_type: None,
            run_ms: None,
            background: false,
        }))
    }
}

/// How a node's elapsed time ends.
#[derive(Clone, Copy)]
enum End {
    /// Its own span (a session or agent).
    Own,
    /// Closed or merged at.
    Closed(i64),
    /// An open epic, issue or PR: at its last activity.
    Open,
    /// Its children's: earliest start to latest end, open if any is.
    Derived,
}

/// One level of a cell's path, as the node it makes.
struct Step {
    kind: NodeKind,
    key: String,
    label: String,
    /// Sorts siblings of one kind.
    order: String,
    end: End,
    span: Option<(i64, i64)>,
    elapsed: Option<i64>,
    progress: Option<Progress>,
    /// Read from the GitHub cache.
    github: bool,
}

impl Step {
    fn new(kind: NodeKind, key: String, label: String, order: String) -> Self {
        Self {
            kind,
            key,
            label,
            order,
            end: End::Derived,
            span: None,
            elapsed: None,
            progress: None,
            github: false,
        }
    }

    /// An epic or issue node from the cache. One the cache doesn't hold is
    /// `<repo> #N`, open: first window to last activity.
    fn issue(kind: NodeKind, gh: &Gh, repo: &str, number: i64) -> Self {
        let cached = gh.issues.get(&(repo.to_string(), number));
        let mut step = Self::new(
            kind,
            number.to_string(),
            match cached {
                Some(i) => format!("#{number} {}", i.title),
                None => format!("{repo} #{number}"),
            },
            format!("{number:020}"),
        );
        step.end = match cached.map(|i| i.closed) {
            Some(Some(Some(at))) => End::Closed(at),
            // Closed with no time given.
            Some(Some(None)) => End::Derived,
            Some(None) | None => End::Open,
        };
        step.progress = cached.and_then(|i| i.progress.clone());
        step.github = cached.is_some();
        step
    }
}

/// A cell's hierarchy path: Repo > Epic > Issue > PR (or Unlinked) >
/// Session > Agent.
fn path(
    atom: &Atom,
    window: &Window,
    gh: &Gh,
    agents: &HashMap<(String, String), Agent>,
) -> Vec<Step> {
    let mut steps = Vec::with_capacity(6);
    match &window.place {
        Place::Unlinked => steps.push(Step::new(
            NodeKind::Unlinked,
            String::new(),
            "Unlinked".into(),
            String::new(),
        )),
        Place::Linked { repo, issue, pr } => {
            steps.push(Step::new(
                NodeKind::Repo,
                repo.clone(),
                repo.clone(),
                repo.clone(),
            ));
            let parent = gh
                .issues
                .get(&(repo.clone(), *issue))
                .and_then(|i| i.parent);
            if let Some(epic) = parent {
                steps.push(Step::issue(NodeKind::Epic, gh, repo, epic));
            }
            steps.push(Step::issue(NodeKind::Issue, gh, repo, *issue));
            if let Some(number) = pr {
                let found = gh
                    .prs
                    .get(&(repo.clone(), *issue))
                    .and_then(|prs| prs.iter().find(|p| p.number == *number));
                let mut step = Step::new(
                    NodeKind::Pr,
                    number.to_string(),
                    format!("PR #{number}"),
                    format!("{number:020}"),
                );
                step.end = match found {
                    Some(GhPr {
                        merged_at: Some(at),
                        ..
                    }) => End::Closed(*at),
                    Some(p) if p.state == "OPEN" => End::Open,
                    _ => End::Derived,
                };
                step.github = true;
                steps.push(step);
            }
        }
    }
    let start = window.span.map_or(0, |s| s.0);
    let mut session = Step::new(
        NodeKind::Session,
        window.key.clone(),
        window.label.clone(),
        format!("{start:020}/{}", window.key),
    );
    session.end = End::Own;
    session.span = window.span;
    steps.push(session);
    let mut agent = match &atom.agent_id {
        None => {
            let mut step = Step::new(
                NodeKind::Agent,
                String::new(),
                "orchestrator".into(),
                String::new(),
            );
            step.span = window.span;
            step
        }
        Some(id) => {
            let a = &agents[&(atom.session_id.clone(), id.clone())];
            let mut step = Step::new(NodeKind::Agent, id.clone(), a.label.clone(), id.clone());
            step.elapsed = a.run_ms;
            step
        }
    };
    agent.end = End::Own;
    steps.push(agent);
    steps
}

/// A cell's Group node under `group_by` (never Hierarchy).
fn group(
    group_by: GroupBy,
    atom: &Atom,
    window: &Window,
    agents: &HashMap<(String, String), Agent>,
) -> Step {
    let (key, label) = match group_by {
        GroupBy::Hierarchy | GroupBy::Day => (atom.day.clone(), atom.day.clone()),
        GroupBy::Model => match &atom.model {
            Some(m) => (m.clone(), m.clone()),
            None => (String::new(), "No requests".into()),
        },
        // Session time is on the orchestrator's cells, so a subagent type's
        // group shows 0 session-level active and waiting; its Agent nodes
        // carry their own.
        GroupBy::AgentType => {
            let kind = match &atom.agent_id {
                None => Some("orchestrator".to_string()),
                Some(id) => agents[&(atom.session_id.clone(), id.clone())]
                    .agent_type
                    .clone(),
            };
            match kind {
                Some(k) => (k.clone(), k),
                None => (String::new(), "Unknown".into()),
            }
        }
        GroupBy::Pr => match &window.place {
            Place::Linked {
                repo, pr: Some(n), ..
            } => (format!("{repo}#{n}"), format!("{repo} #{n}")),
            _ => (String::new(), "No PR".into()),
        },
    };
    Step::new(NodeKind::Group, key.clone(), label, key)
}

/// A node while the tree is built.
struct Node {
    step: Step,
    metrics: Metrics,
    children: Vec<Node>,
    index: HashMap<(NodeKind, String), usize>,
}

impl Node {
    fn root() -> Self {
        Self::new(Step::new(
            NodeKind::Root,
            String::new(),
            "All".into(),
            String::new(),
        ))
    }

    fn new(step: Step) -> Self {
        Self {
            step,
            metrics: Metrics::default(),
            children: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Adds `share` to this node and to each node along `steps`, creating
    /// the missing ones.
    fn insert(&mut self, steps: Vec<Step>, share: &Share) {
        self.add(share);
        let mut node = self;
        for step in steps {
            let key = (step.kind, step.key.clone());
            let i = match node.index.get(&key) {
                Some(&i) => i,
                None => {
                    node.children.push(Node::new(step));
                    node.index.insert(key, node.children.len() - 1);
                    node.children.len() - 1
                }
            };
            node = &mut node.children[i];
            node.add(share);
        }
    }

    fn add(&mut self, s: &Share) {
        let m = &mut self.metrics;
        if let Some(c) = s.cost_usd {
            m.cost_usd += c;
        }
        m.unpriced += s.unpriced;
        let t = &mut m.tokens;
        for (sum, n) in [
            &mut t.input,
            &mut t.output,
            &mut t.cache_read,
            &mut t.cache_write,
        ]
        .into_iter()
        .zip(s.tokens)
        {
            *sum += n;
        }
        m.agent_ms += s.agent_ms;
        m.approximate |= s.approximate;
        let [active, wait] = if self.step.kind == NodeKind::Agent {
            s.own
        } else {
            s.line
        };
        m.active_ms += active;
        m.wait_ms += wait;
        m.sessions += s.sessions;
        m.agents += s.agents;
        m.rotations += s.rotations;
    }

    /// The finished node, its span and whether it is open; closed issues
    /// and merged PRs go on `tally`.
    fn finish(self, tally: &mut Tally) -> (AuditNode, Option<(i64, i64)>, bool) {
        let Node {
            step,
            mut metrics,
            mut children,
            ..
        } = self;
        children.sort_by(|a, b| {
            (a.step.kind as u8, &a.step.order).cmp(&(b.step.kind as u8, &b.step.order))
        });
        let finished: Vec<_> = children.into_iter().map(|c| c.finish(tally)).collect();
        let start = finished.iter().filter_map(|f| f.1.map(|s| s.0)).min();
        let end = finished.iter().filter_map(|f| f.1.map(|s| s.1)).max();
        let any_open = finished.iter().any(|f| f.2);
        let (span, open) = match step.end {
            End::Own => (step.span, false),
            End::Closed(at) => (start.map(|s| (s, at)), false),
            End::Open => (start.zip(end), true),
            End::Derived => (start.zip(end), any_open),
        };
        metrics.elapsed_ms = step.elapsed.or(span.map(|(s, e)| (e - s).max(0)));
        metrics.open = open;
        metrics.progress = step.progress;
        if let End::Closed(_) = step.end {
            match step.kind {
                NodeKind::Issue => tally.issues.push((metrics.elapsed_ms, metrics.cost_usd)),
                NodeKind::Pr => tally.merged.push(metrics.cost_usd),
                _ => {}
            }
        }
        let node = AuditNode {
            kind: step.kind,
            key: step.key,
            label: step.label,
            github: step.github,
            metrics,
            children: finished.into_iter().map(|f| f.0).collect(),
        };
        (node, span, open)
    }
}

/// The closed issues (elapsed, $) and merged PRs ($) of a hierarchy.
#[derive(Default)]
struct Tally {
    issues: Vec<(Option<i64>, f64)>,
    merged: Vec<f64>,
}

impl Tally {
    fn headline(self) -> Headline {
        let mut elapsed: Vec<i64> = self.issues.iter().filter_map(|i| i.0).collect();
        elapsed.sort_unstable();
        let n = elapsed.len();
        let median = match n {
            0 => None,
            _ if n % 2 == 1 => Some(elapsed[n / 2]),
            _ => Some((elapsed[n / 2 - 1] + elapsed[n / 2]) / 2),
        };
        let mean = |costs: &[f64]| {
            (!costs.is_empty()).then(|| costs.iter().sum::<f64>() / costs.len() as f64)
        };
        let issue_costs: Vec<f64> = self.issues.iter().map(|i| i.1).collect();
        Headline {
            issues_closed: self.issues.len() as i64,
            median_elapsed_ms: median,
            usd_per_closed_issue: mean(&issue_costs),
            usd_per_merged_pr: mean(&self.merged),
            cloud_uncounted: true,
        }
    }
}
