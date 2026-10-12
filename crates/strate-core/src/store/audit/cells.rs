//! The `audit_cells` in a filter, with the `/rename` windows and subagents
//! they belong to, each resolved once.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, Row};

use super::gh::{Gh, GhPr};
use super::{AuditFilter, Tokens};
use crate::names::{ParsedName, parse_name};
use crate::store::Result;
use crate::time::parse_timestamp;

/// The cells in a filter, with the windows and subagents they belong to.
pub(crate) struct Cells {
    /// In day order, the orchestrator's first.
    pub(crate) list: Vec<Cell>,
    windows: Windows,
    agents: HashMap<(String, String), Agent>,
}

/// One cell in the filter: where it sits and what it adds.
pub(crate) struct Cell {
    pub(crate) session_id: String,
    /// `None`: the orchestrator.
    pub(crate) agent_id: Option<String>,
    /// Its index in [`Cells`]' windows; see [`Cells::window`].
    window: usize,
    pub(crate) day: String,
    pub(crate) model: Option<String>,
    pub(crate) share: Share,
}

/// What one cell adds to each node above it.
#[derive(Default)]
pub(crate) struct Share {
    pub(crate) cost_usd: Option<f64>,
    pub(crate) unpriced: i64,
    pub(crate) tokens: Tokens,
    pub(crate) agent_ms: i64,
    pub(crate) approximate: bool,
    /// Active and waiting on the session line: the orchestrator's cells only.
    pub(crate) line: Time,
    /// The cell's own active and waiting, for its Agent node.
    pub(crate) own: Time,
    pub(crate) sessions: i64,
    pub(crate) agents: i64,
    pub(crate) rotations: i64,
}

/// Active time and time waiting on you.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Time {
    pub(crate) active_ms: i64,
    pub(crate) wait_ms: i64,
}

/// One `audit_cells` row, read by column name.
struct CellRow {
    session_id: String,
    agent_id: Option<String>,
    segment_id: Option<i64>,
    day: String,
    model: Option<String>,
    /// The subagent's wall time in the cell.
    wall_ms: i64,
    own: Time,
    cost_usd: Option<f64>,
    unpriced: i64,
    tokens: Tokens,
}

impl CellRow {
    fn read(r: &Row) -> Result<Self> {
        Ok(Self {
            session_id: r.get("session_id")?,
            agent_id: r.get("agent_id")?,
            segment_id: r.get("segment_id")?,
            day: r.get("day")?,
            model: r.get("model")?,
            wall_ms: r.get("wall_ms")?,
            own: Time {
                active_ms: r.get("active_ms")?,
                wait_ms: r.get("wait_ms")?,
            },
            cost_usd: r.get("cost_usd")?,
            unpriced: r.get("unpriced")?,
            tokens: Tokens {
                input: r.get("input_tokens")?,
                output: r.get("output_tokens")?,
                cache_read: r.get("cache_read_tokens")?,
                cache_write: r.get("cache_write_tokens")?,
            },
        })
    }
}

impl Cells {
    /// Cells come in day order, the orchestrator's first, so a window or
    /// subagent counts on its first cell in the filter. Idle-only cells are
    /// left out: they add nothing, and would show a window on a day it only
    /// sat open.
    pub(crate) fn load(conn: &Connection, gh: &Gh, filter: &AuditFilter) -> Result<Self> {
        let (from, to) = (day(&filter.from)?, day(&filter.to)?);
        let mut out = Self {
            list: Vec::new(),
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
            let row = CellRow::read(r)?;
            let window = out.windows.get(conn, gh, &row.session_id, row.segment_id)?;
            let w = &out.windows.list[window];
            let wanted = |repo: &String| filter.repos.contains(repo);
            if !filter.repos.is_empty() && !w.repo.as_ref().is_some_and(wanted) {
                continue;
            }
            let first_in_window = seen_windows.insert(window);
            let mut share = Share {
                cost_usd: row.cost_usd,
                unpriced: row.unpriced,
                tokens: row.tokens,
                own: row.own,
                sessions: first_in_window.into(),
                rotations: (first_in_window && w.rotation).into(),
                ..Share::default()
            };
            match &row.agent_id {
                None => share.line = share.own,
                Some(agent_id) => {
                    let key = (row.session_id.clone(), agent_id.clone());
                    let agent = out
                        .agents
                        .entry(key.clone())
                        .or_insert_with(|| Agent::unknown(agent_id));
                    share.agent_ms = row.wall_ms;
                    share.approximate = agent.background;
                    share.agents = seen_agents.insert(key).into();
                }
            }
            out.list.push(Cell {
                session_id: row.session_id,
                agent_id: row.agent_id,
                window,
                day: row.day,
                model: row.model,
                share,
            });
        }
        Ok(out)
    }

    /// The window `cell` belongs to.
    pub(crate) fn window(&self, cell: &Cell) -> &Window {
        &self.windows.list[cell.window]
    }

    /// The subagent `agent_id` of `cell`'s session. [`Cells::load`] gives
    /// every subagent cell an entry, so the lookup misses only for a cell
    /// that did not come from it.
    pub(crate) fn agent(&self, cell: &Cell, agent_id: &str) -> Option<&Agent> {
        self.agents
            .get(&(cell.session_id.clone(), agent_id.to_string()))
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

/// Where a window's work belongs.
pub(crate) enum Place {
    Unlinked,
    Linked {
        repo: String,
        issue: i64,
        pr: Option<i64>,
    },
}

/// A `/rename` window (or a session with none).
pub(crate) struct Window {
    pub(crate) key: String,
    pub(crate) label: String,
    /// The repo its name parses to, linked or not.
    repo: Option<String>,
    /// Named `-sN`, any N: a rotation is a `-sN` window, not "windows - 1".
    rotation: bool,
    pub(crate) place: Place,
    /// Its start to the next window's start, or to the session's last
    /// activity.
    pub(crate) span: Option<(i64, i64)>,
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
    let issue = p.issue as i64;
    let prs = gh.prs(&p.repo, issue);
    let only = match prs {
        [only] => Some(only.number),
        _ => None,
    };
    Place::Linked {
        pr: linked_pr(prs, links, &p.repo).or(only),
        repo: p.repo.clone(),
        issue,
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
pub(crate) struct Agent {
    pub(crate) label: String,
    pub(crate) agent_type: Option<String>,
    pub(crate) run_ms: Option<i64>,
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
