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
//! Cells with no activity, requests or subagent run (a window sitting open
//! overnight) are left out, so they put no node on a day.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
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
    let mut tree = Node::root();
    for atom in &cells.atoms {
        let window = &cells.windows.list[atom.window];
        let mut steps = Vec::with_capacity(7);
        if filter.group_by != GroupBy::Hierarchy {
            steps.push(group(filter.group_by, atom, window, &gh, &cells.agents));
        }
        steps.extend(path(atom, window, &gh, &cells.agents));
        tree.insert(steps, &atom.share);
    }
    Ok(Audit {
        root: tree.finish().0,
        headline: headline(conn, &gh, &cells)?,
        github_errors: github_errors(conn)?,
    })
}

/// Every repo whose latest GitHub refresh failed, whatever the filter.
fn github_errors(conn: &Connection) -> Result<Vec<GithubError>> {
    conn.prepare_cached("SELECT slug, error FROM gh_repos WHERE error IS NOT NULL ORDER BY slug")?
        .query_map([], |r| {
            Ok(GithubError {
                repo: r.get(0)?,
                error: r.get(1)?,
            })
        })?
        .collect()
}

/// The cells in a filter, with the windows and subagents they belong to.
struct Cells {
    atoms: Vec<Atom>,
    windows: Windows,
    agents: HashMap<(String, String), Agent>,
}

impl Cells {
    /// Cells come in day order, the orchestrator's first, so a window or
    /// subagent counts on its first cell in the filter. Idle-only cells are
    /// left out: they add nothing, and would show a window on a day it only
    /// sat open.
    fn load(conn: &Connection, gh: &Gh, filter: &AuditFilter) -> Result<Self> {
        let (from, to) = (day(&filter.from)?, day(&filter.to)?);
        let mut out = Self {
            atoms: Vec::new(),
            windows: Windows {
                named: Named::load_all(conn, from, to)?,
                ..Windows::default()
            },
            agents: Agent::load_all(conn, from, to)?,
        };
        let mut seen_windows: HashSet<usize> = HashSet::new();
        let mut seen_agents: HashSet<(String, String)> = HashSet::new();
        // A cell with a NULL day (a request with no usable timestamp) is in
        // no range: `day BETWEEN` is never true for NULL.
        let mut stmt = conn.prepare_cached(
            "SELECT session_id, agent_id, segment_id, day, model, wall_ms, active_ms, wait_ms,
                 cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
                 cache_write_tokens
             FROM audit_cells WHERE day BETWEEN ?1 AND ?2
                 AND NOT (active_ms = 0 AND wait_ms = 0 AND cost_usd IS NULL AND unpriced = 0
                     AND input_tokens + output_tokens + cache_read_tokens
                         + cache_write_tokens = 0
                     AND (agent_id IS NULL OR wall_ms = 0))
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
                    let agent = out
                        .agents
                        .entry(key.clone())
                        .or_insert_with(|| Agent::unknown(agent_id));
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
    /// Closed as completed (`stateReason`), not as not planned or a
    /// duplicate: only these count in the headline.
    completed: bool,
    parent: Option<i64>,
    progress: Option<Progress>,
}

struct GhPr {
    /// The PR's own repo slug, from its URL; else the issue's.
    repo: String,
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
                "SELECT repo, number, title, state, closed_at, parent, sub_total, sub_completed,
                     state_reason
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
                        completed: r.get::<_, Option<String>>(8)?.as_deref() == Some("COMPLETED"),
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
            let issue_repo: String = r.get(0)?;
            let url: String = r.get(5)?;
            Ok((
                (issue_repo.clone(), r.get(1)?),
                GhPr {
                    repo: url_repo(&url).map_or(issue_repo, str::to_string),
                    number: r.get(2)?,
                    state: r.get(3)?,
                    merged_at: at(r.get(4)?),
                    url,
                },
            ))
        })? {
            let (key, pr) = row?;
            prs.entry(key).or_default().push(pr);
        }
        Ok(Self { issues, prs })
    }

    /// PR `number` among those closing `repo`#`issue`.
    fn pr(&self, repo: &str, issue: i64, number: i64) -> Option<&GhPr> {
        self.prs
            .get(&(repo.to_string(), issue))?
            .iter()
            .find(|p| p.number == number)
    }
}

/// The repo name in a `https://github.com/<owner>/<name>/pull/<n>` URL.
fn url_repo(url: &str) -> Option<&str> {
    let path = url.strip_prefix("https://")?;
    match path.split('/').collect::<Vec<_>>()[..] {
        [_host, _owner, name, "pull", _number] => Some(name),
        _ => None,
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
    /// The filter's segments by id, read in one query.
    named: HashMap<i64, Named>,
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
        // With no window, the session's id and start.
        let named = segment
            .and_then(|id| self.named.remove(&id))
            .unwrap_or_else(|| Named {
                name: session_id.to_string(),
                fallback: true,
                start: session.start,
                next: None,
            });
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
    /// Its first record.
    start: Option<i64>,
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
        let (start, last): (Option<String>, Option<String>) = conn
            .prepare_cached(
                "SELECT (SELECT started_at FROM sessions WHERE session_id = ?1),
                     (SELECT max(timestamp) FROM events
                      WHERE session_id = ?1 AND timestamp IS NOT NULL
                        AND ifnull(type, '') NOT IN ('custom-title', 'ai-title', 'cost-state'))",
            )?
            .query_row([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(Self {
            links,
            start: start.as_deref().and_then(parse_timestamp),
            last: last.as_deref().and_then(parse_timestamp),
        })
    }
}

/// A window's name (merge-resolved) and where it starts and the next one
/// starts.
struct Named {
    name: String,
    /// A cwd/branch fallback, not a name the user set.
    fallback: bool,
    start: Option<i64>,
    next: Option<i64>,
}

impl Named {
    /// Every segment of the sessions with cells on days `from..=to`, by id,
    /// in one query (one pass over the recursive `workstream_roots`).
    fn load_all(conn: &Connection, from: &str, to: &str) -> Result<HashMap<i64, Self>> {
        let at = |s: Option<String>| s.as_deref().and_then(parse_timestamp);
        conn.prepare_cached(
            "SELECT g.id, w.name, w.fallback, g.started_at, g.next
             FROM (
                 SELECT id, workstream_id, started_at,
                     lead(started_at) OVER (PARTITION BY session_id ORDER BY start_offset)
                         AS next
                 FROM segments
                 WHERE session_id IN (
                     SELECT session_id FROM audit_cells WHERE day BETWEEN ?1 AND ?2
                 )
             ) AS g
             JOIN workstream_roots AS r ON r.id = g.workstream_id
             JOIN workstreams AS w ON w.id = r.root",
        )?
        .query_map([from, to], |r| {
            Ok((
                r.get(0)?,
                Self {
                    name: r.get(1)?,
                    fallback: r.get(2)?,
                    start: at(r.get(3)?),
                    next: at(r.get(4)?),
                },
            ))
        })?
        .collect()
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
    /// Every subagent of the sessions with subagent cells on days
    /// `from..=to`, by (session, agent id), in one query: each parent
    /// session's tool_use blocks are read once for the background flags.
    fn load_all(
        conn: &Connection,
        from: &str,
        to: &str,
    ) -> Result<HashMap<(String, String), Self>> {
        conn.prepare_cached(
            "WITH wanted (session_id) AS (
                 SELECT DISTINCT session_id FROM audit_cells
                 WHERE day BETWEEN ?1 AND ?2 AND agent_id IS NOT NULL
             ),
             dispatch (child, tool_use_id, parent_session) AS (
                 SELECT d.child, d.tool_use_id, p.session_id FROM edges AS d
                 JOIN agents AS c ON c.id = d.child
                 JOIN agents AS p ON p.id = d.parent
                 WHERE d.kind = 'dispatch' AND c.session_id IN (SELECT session_id FROM wanted)
             ),
             background (id) AS (
                 SELECT json_extract(b.value, '$.id')
                 FROM events AS e, json_each(e.raw, '$.message.content') AS b
                 WHERE e.session_id IN (SELECT parent_session FROM dispatch)
                   AND e.tool_uses IS NOT NULL AND b.type = 'object'
                   AND json_extract(b.value, '$.input.run_in_background') IS 1
             )
             SELECT a.session_id, a.agent_id,
                 coalesce(a.registry_name, a.name, a.description, a.agent_type), a.agent_type,
                 a.run_ms,
                 EXISTS (SELECT 1 FROM dispatch AS d
                     WHERE d.child = a.id AND d.tool_use_id IN (SELECT id FROM background))
             FROM agents AS a
             WHERE a.session_id IN (SELECT session_id FROM wanted) AND a.agent_id IS NOT NULL",
        )?
        .query_map([from, to], |r| {
            let agent_id: String = r.get(1)?;
            Ok((
                (r.get(0)?, agent_id.clone()),
                Self {
                    label: r.get::<_, Option<String>>(2)?.unwrap_or(agent_id),
                    agent_type: r.get(3)?,
                    run_ms: r.get(4)?,
                    background: r.get(5)?,
                },
            ))
        })?
        .collect()
    }

    /// A subagent with cells but no `agents` row.
    fn unknown(agent_id: &str) -> Self {
        Self {
            label: agent_id.to_string(),
            agent_type: None,
            run_ms: None,
            background: false,
        }
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
                let found = gh.pr(repo, *issue, *number);
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
    gh: &Gh,
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
        // Keyed by the PR's own repo, which may not be the issue's.
        GroupBy::Pr => match &window.place {
            Place::Linked {
                repo,
                issue,
                pr: Some(n),
            } => {
                let repo = gh.pr(repo, *issue, *n).map_or(repo, |p| &p.repo);
                (format!("{repo}#{n}"), format!("{repo} #{n}"))
            }
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

    /// The finished node, its span and whether it is open.
    fn finish(self) -> (AuditNode, Option<(i64, i64)>, bool) {
        let Node {
            step,
            mut metrics,
            mut children,
            ..
        } = self;
        children.sort_by(|a, b| {
            (a.step.kind as u8, &a.step.order).cmp(&(b.step.kind as u8, &b.step.order))
        });
        let finished: Vec<_> = children.into_iter().map(Node::finish).collect();
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

/// The headline over the cells in the filter: each issue closed as
/// completed and each merged PR that has work there, with that work's $.
/// An issue's elapsed is its whole life, from its first window ever.
fn headline(conn: &Connection, gh: &Gh, cells: &Cells) -> Result<Headline> {
    let mut issues: HashMap<(&str, i64), f64> = HashMap::new();
    let mut merged: HashMap<(&str, i64), f64> = HashMap::new();
    for atom in &cells.atoms {
        let Place::Linked { repo, issue, pr } = &cells.windows.list[atom.window].place else {
            continue;
        };
        let usd = atom.share.cost_usd.unwrap_or(0.0);
        *issues.entry((repo, *issue)).or_default() += usd;
        // A PR closing several issues counts once, with all their $.
        if let Some(pr) = pr.and_then(|n| gh.pr(repo, *issue, n))
            && pr.merged_at.is_some()
        {
            *merged.entry((&pr.repo, pr.number)).or_default() += usd;
        }
    }
    let starts = first_windows(conn)?;
    let mut elapsed = Vec::new();
    let mut closed = Vec::new();
    for ((repo, number), usd) in issues {
        let key = (repo.to_string(), number);
        // Closed at a known time, as completed.
        let closed_at = gh.issues.get(&key).filter(|i| i.completed);
        let Some(at) = closed_at.and_then(|i| i.closed.flatten()) else {
            continue;
        };
        elapsed.extend(starts.get(&key).map(|start| (at - start).max(0)));
        closed.push(usd);
    }
    elapsed.sort_unstable();
    let n = elapsed.len();
    let mean =
        |usd: Vec<f64>| (!usd.is_empty()).then(|| usd.iter().sum::<f64>() / usd.len() as f64);
    Ok(Headline {
        issues_closed: closed.len() as i64,
        median_elapsed_ms: match n {
            0 => None,
            _ if n % 2 == 1 => Some(elapsed[n / 2]),
            _ => Some((elapsed[n / 2 - 1] + elapsed[n / 2]) / 2),
        },
        usd_per_closed_issue: mean(closed),
        usd_per_merged_pr: mean(merged.into_values().collect()),
        cloud_uncounted: true,
    })
}

/// The start of each issue's first user-named window ever, by (repo, issue).
fn first_windows(conn: &Connection) -> Result<HashMap<(String, i64), i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT w.name, g.started_at FROM segments AS g
         JOIN workstream_roots AS r ON r.id = g.workstream_id
         JOIN workstreams AS w ON w.id = r.root
         WHERE NOT w.fallback AND g.started_at IS NOT NULL",
    )?;
    let mut rows = stmt.query([])?;
    let mut first: HashMap<(String, i64), i64> = HashMap::new();
    while let Some(r) = rows.next()? {
        let (Some(parsed), Some(start)) = (
            parse_name(&r.get::<_, String>(0)?),
            parse_timestamp(&r.get::<_, String>(1)?),
        ) else {
            continue;
        };
        first
            .entry((parsed.repo, parsed.issue as i64))
            .and_modify(|s| *s = (*s).min(start))
            .or_insert(start);
    }
    Ok(first)
}
