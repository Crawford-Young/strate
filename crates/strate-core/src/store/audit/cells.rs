//! The `audit_cells` in a filter, with the charges, sessions, Session rows
//! and subagents they belong to, each resolved once.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::{Connection, Row};

use super::gh::{Gh, GhPr};
use super::{AuditFilter, Tokens};
use crate::charge::{Charge, ChargeKind};
use crate::store::Result;
use crate::time::parse_timestamp;

/// The cells in a filter, with what they belong to.
pub(crate) struct Cells {
    /// In day order, the orchestrator's first.
    pub(crate) list: Vec<Cell>,
    /// The repos the range's cells are charged in, whatever the repo
    /// filter.
    pub(crate) repos: BTreeSet<String>,
    /// Some cell in the range is in no repo.
    pub(crate) unlinked: bool,
    charges: Charges,
    rows: Vec<SessionRow>,
    row_index: HashMap<RowKey, usize>,
    sessions: HashMap<String, Session>,
    agents: HashMap<(String, String), Agent>,
}

/// One cell in the filter: where it sits and what it adds.
pub(crate) struct Cell {
    pub(crate) session_id: String,
    /// `None`: the orchestrator.
    pub(crate) agent_id: Option<String>,
    /// Its index in [`Cells`]' Session rows; see [`Cells::row`].
    row: usize,
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
    /// Active and waiting on the session line: the orchestrator's cells only.
    pub(crate) line: Time,
    /// The cell's own active and waiting, for its Agent node.
    pub(crate) own: Time,
    pub(crate) sessions: i64,
    pub(crate) agents: i64,
}

/// Active time and time waiting on you.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Time {
    pub(crate) active_ms: i64,
    pub(crate) wait_ms: i64,
    /// Of `wait_ms`, on questions to you rather than the next prompt.
    pub(crate) ask_ms: i64,
}

/// One `audit_cells` row, read by column name.
struct CellRow {
    session_id: String,
    agent_id: Option<String>,
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
            day: r.get("day")?,
            model: r.get("model")?,
            wall_ms: r.get("wall_ms")?,
            own: Time {
                active_ms: r.get("active_ms")?,
                wait_ms: r.get("wait_ms")?,
                ask_ms: r.get("ask_ms")?,
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
    /// Cells come in day order, the orchestrator's first, so a session or
    /// subagent counts on its first cell in the filter. Idle-only cells are
    /// left out: they add nothing, and would show a session on a day it
    /// only sat open.
    pub(crate) fn load(conn: &Connection, gh: &Gh, filter: &AuditFilter) -> Result<Self> {
        let (from, to) = (day(&filter.from)?, day(&filter.to)?);
        let mut out = Self {
            list: Vec::new(),
            repos: BTreeSet::new(),
            unlinked: false,
            charges: Charges::load(conn)?,
            rows: Vec::new(),
            row_index: HashMap::new(),
            sessions: HashMap::new(),
            agents: Agent::load_all(conn, from, to)?,
        };
        let mut seen_sessions: HashSet<(String, Option<i64>)> = HashSet::new();
        let mut seen_agents: HashSet<(String, String)> = HashSet::new();
        // A cell with a NULL day (a request with no usable timestamp) is in
        // no range: `day BETWEEN` is never true for NULL.
        let mut stmt = conn.prepare_cached(
            "SELECT session_id, agent_id, day, model, wall_ms, active_ms, wait_ms, ask_ms,
                 cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
                 cache_write_tokens
             FROM audit_cells WHERE day BETWEEN ?1 AND ?2
                 AND NOT (active_ms = 0 AND wait_ms = 0 AND cost_usd IS NULL AND unpriced = 0
                     AND input_tokens + output_tokens + cache_read_tokens
                         + cache_write_tokens = 0
                     AND (agent_id IS NULL OR wall_ms = 0))
             ORDER BY day, agent_id IS NOT NULL, agent_id, session_id, model, id",
        )?;
        let mut rows = stmt.query([from, to])?;
        while let Some(r) = rows.next()? {
            let row = CellRow::read(r)?;
            let charge = out.charges.of(&row.session_id, row.agent_id.as_deref());
            let at = out.row(conn, gh, &row.session_id, charge)?;
            match out.rows[at].place.repo() {
                Some(repo) if !out.repos.contains(repo) => {
                    out.repos.insert(repo.to_string());
                }
                Some(_) => {}
                None => out.unlinked = true,
            }
            let slug = out.rows[at].place.repo().unwrap_or("");
            if !filter.repos.is_empty() && !filter.repos.iter().any(|r| r == slug) {
                continue;
            }
            let first_in_row = seen_sessions.insert((row.session_id.clone(), charge));
            let mut share = Share {
                cost_usd: row.cost_usd,
                unpriced: row.unpriced,
                tokens: row.tokens,
                own: row.own,
                sessions: first_in_row.into(),
                ..Share::default()
            };
            match &row.agent_id {
                None => share.line = share.own,
                Some(agent_id) => {
                    let key = (row.session_id.clone(), agent_id.clone());
                    out.agents
                        .entry(key.clone())
                        .or_insert_with(|| Agent::unknown(agent_id));
                    share.agent_ms = row.wall_ms;
                    share.agents = seen_agents.insert(key).into();
                }
            }
            out.list.push(Cell {
                session_id: row.session_id,
                agent_id: row.agent_id,
                row: at,
                day: row.day,
                model: row.model,
                share,
            });
        }
        Ok(out)
    }

    /// The index of the Session row of `session_id`'s chain in `charge`,
    /// placed once.
    fn row(
        &mut self,
        conn: &Connection,
        gh: &Gh,
        session_id: &str,
        charge: Option<i64>,
    ) -> Result<usize> {
        let chain = self.session_of(conn, session_id)?.chain.clone();
        let key = (chain, charge);
        if let Some(&at) = self.row_index.get(&key) {
            return Ok(at);
        }
        let charge = charge.and_then(|id| self.charges.by_id.get(&id));
        let place = match charge {
            Some(c) if c.kind == ChargeKind::Issue => {
                let links = row_links(conn, self.charges.row_sessions(&key))?;
                place(gh, Some(c), &links)
            }
            c => place(gh, c, &[]),
        };
        let charge = charge.map(Charge::key).unwrap_or_default();
        self.rows.push(SessionRow {
            key: format!("{}/{charge}", key.0),
            charge,
            place,
        });
        self.row_index.insert(key, self.rows.len() - 1);
        Ok(self.rows.len() - 1)
    }

    /// Session `session_id`, read once.
    fn session_of(&mut self, conn: &Connection, session_id: &str) -> Result<&Session> {
        Ok(match self.sessions.entry(session_id.to_string()) {
            Entry::Occupied(found) => found.into_mut(),
            Entry::Vacant(slot) => slot.insert(Session::load(conn, session_id)?),
        })
    }

    /// The Session row `cell` belongs to.
    pub(crate) fn row_of(&self, cell: &Cell) -> &SessionRow {
        &self.rows[cell.row]
    }

    /// The session `cell` is from. [`Cells::load`] read every cell's.
    pub(crate) fn session(&self, cell: &Cell) -> &Session {
        &self.sessions[&cell.session_id]
    }

    /// The subagent `agent_id` of `cell`'s session. [`Cells::load`] gives
    /// every subagent cell an entry, so the lookup misses only for a cell
    /// that did not come from it.
    pub(crate) fn agent(&self, cell: &Cell, agent_id: &str) -> Option<&Agent> {
        self.agents
            .get(&(cell.session_id.clone(), agent_id.to_string()))
    }

    /// The repo of the agent `agent_id` (`None`: the orchestrator) of
    /// `session_id`'s effective charge; `None` for a topic or no charge.
    pub(crate) fn repo_of(&self, session_id: &str, agent_id: Option<&str>) -> Option<&str> {
        let charge = self.charges.of(session_id, agent_id)?;
        self.charges.by_id.get(&charge).and_then(charge_repo)
    }

    /// Each issue's first start: of the earliest session with an agent
    /// charged to it, to its PR (closing it) or, for an epic, to it.
    pub(crate) fn issue_starts(&self, gh: &Gh) -> HashMap<(String, i64), i64> {
        let mut out: HashMap<(String, i64), i64> = HashMap::new();
        for (id, &start) in &self.charges.first {
            let Some(c) = self.charges.by_id.get(id) else {
                continue;
            };
            let place = place(gh, Some(c), &[]);
            let Some((repo, issue, _)) = place.issue() else {
                continue;
            };
            out.entry((repo.to_string(), issue))
                .and_modify(|s| *s = (*s).min(start))
                .or_insert(start);
        }
        out
    }
}

/// `s` when it is a `YYYY-MM-DD` day.
pub(crate) fn day(s: &str) -> Result<&str> {
    match s.parse::<jiff::civil::Date>() {
        Ok(d) if d.to_string() == s => Ok(s),
        _ => Err(rusqlite::Error::ToSqlConversionFailure(
            format!("not a YYYY-MM-DD day: {s:?}").into(),
        )),
    }
}

/// The repo a charge is in: none for a topic.
fn charge_repo(c: &Charge) -> Option<&str> {
    match c.kind {
        ChargeKind::Topic => None,
        _ => c.repo.as_deref(),
    }
}

/// A Session row's chain and charge (`None`: Unlinked).
type RowKey = (String, Option<i64>);

/// Every agent's effective charge, read once: `agent_charges` is a
/// recursive view over every agent, so it is read whole, never per cell.
struct Charges {
    /// Per agent (session, agent id; `None`: the orchestrator), its charge
    /// (`None`: Unlinked).
    agents: HashMap<(String, Option<String>), Option<i64>>,
    by_id: HashMap<i64, Charge>,
    /// Per (chain, charge), its sessions and their starts.
    rows: HashMap<RowKey, Vec<(Option<i64>, String)>>,
    /// Per charge, the earliest start of a session with an agent in it.
    first: HashMap<i64, i64>,
}

impl Charges {
    fn load(conn: &Connection) -> Result<Self> {
        let effective: HashMap<i64, Option<i64>> = conn
            .prepare_cached("SELECT agent, charge FROM agent_charges")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;
        let mut out = Self {
            agents: HashMap::new(),
            by_id: HashMap::new(),
            rows: HashMap::new(),
            first: HashMap::new(),
        };
        let mut stmt = conn.prepare_cached(
            "SELECT a.id, a.session_id, a.agent_id, ifnull(s.process_root, a.session_id),
                 s.started_at
             FROM agents AS a LEFT JOIN sessions AS s ON s.session_id = a.session_id",
        )?;
        let mut rows = stmt.query([])?;
        let mut seen: HashSet<(String, Option<i64>, String)> = HashSet::new();
        while let Some(r) = rows.next()? {
            let session: String = r.get(1)?;
            let chain: String = r.get(3)?;
            let started: Option<String> = r.get(4)?;
            let start = started.as_deref().and_then(parse_timestamp);
            let charge = effective.get(&r.get(0)?).copied().flatten();
            out.agents.insert((session.clone(), r.get(2)?), charge);
            if seen.insert((chain.clone(), charge, session.clone())) {
                out.rows
                    .entry((chain, charge))
                    .or_default()
                    .push((start, session));
            }
            if let (Some(c), Some(start)) = (charge, start) {
                out.first
                    .entry(c)
                    .and_modify(|s| *s = (*s).min(start))
                    .or_insert(start);
            }
        }
        let mut stmt = conn.prepare_cached("SELECT id, kind, repo, number, topic FROM charges")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let kind: String = r.get(1)?;
            let Some(kind) = ChargeKind::parse(&kind) else {
                continue;
            };
            let charge = Charge {
                kind,
                repo: r.get(2)?,
                number: r.get::<_, Option<i64>>(3)?.map(|n| n as u64),
                topic: r.get(4)?,
            };
            out.by_id.insert(r.get(0)?, charge);
        }
        Ok(out)
    }

    /// The effective charge of agent `agent_id` of `session_id`; `None`
    /// (Unlinked) for an agent with no row, as `charge_costs` reads it.
    fn of(&self, session_id: &str, agent_id: Option<&str>) -> Option<i64> {
        self.agents
            .get(&(session_id.to_string(), agent_id.map(str::to_string)))
            .copied()
            .flatten()
    }

    /// The sessions of Session row `key` (a chain in a charge), newest
    /// first: by start (none last), then id.
    fn row_sessions(&self, key: &RowKey) -> Vec<&str> {
        let mut sessions: Vec<&(Option<i64>, String)> = self
            .rows
            .get(key)
            .map(|v| v.iter().collect())
            .unwrap_or_default();
        sessions.sort_by(|a, b| {
            (
                a.0.is_none(),
                std::cmp::Reverse(a.0),
                std::cmp::Reverse(&a.1),
            )
                .cmp(&(
                    b.0.is_none(),
                    std::cmp::Reverse(b.0),
                    std::cmp::Reverse(&b.1),
                ))
        });
        sessions.into_iter().map(|(_, s)| s.as_str()).collect()
    }
}

/// Where a Session row's work belongs, by its charge.
#[derive(Clone)]
pub(crate) enum Place {
    /// No charge.
    Unlinked,
    Topic(String),
    Repo(String),
    Epic {
        repo: String,
        number: i64,
    },
    /// An issue, or a PR the cache knows the issue of, with that PR.
    Issue {
        repo: String,
        issue: i64,
        pr: Option<i64>,
    },
    /// A PR the cache doesn't hold.
    Pr {
        repo: String,
        number: i64,
    },
}

impl Place {
    /// The repo it is in; `None` for a topic or no charge.
    pub(crate) fn repo(&self) -> Option<&str> {
        match self {
            Place::Unlinked | Place::Topic(_) => None,
            Place::Repo(repo)
            | Place::Epic { repo, .. }
            | Place::Issue { repo, .. }
            | Place::Pr { repo, .. } => Some(repo),
        }
    }

    /// The issue it is work on (an epic is one too), and that work's PR:
    /// what can close or merge as an outcome.
    pub(crate) fn issue(&self) -> Option<(&str, i64, Option<i64>)> {
        match self {
            Place::Issue { repo, issue, pr } => Some((repo, *issue, *pr)),
            Place::Epic { repo, number } => Some((repo, *number, None)),
            _ => None,
        }
    }
}

/// A Session row: a `/clear` chain's sessions within one charge.
pub(crate) struct SessionRow {
    /// `<chain>/<charge key>`, `<chain>/` with no charge.
    pub(crate) key: String,
    /// Its charge key, `""` with none.
    pub(crate) charge: String,
    pub(crate) place: Place,
}

/// Where work charged `charge` belongs: a PR charge under the issue it
/// closes when the cache holds one; an issue under the PR its row links
/// (`links`, newest first), else the issue's only PR.
fn place(gh: &Gh, charge: Option<&Charge>, links: &[PrLink]) -> Place {
    let Some(c) = charge else {
        return Place::Unlinked;
    };
    let numbered = c.repo.clone().zip(c.number.map(|n| n as i64));
    match (c.kind, numbered) {
        (ChargeKind::Topic, _) => c.topic.clone().map_or(Place::Unlinked, Place::Topic),
        (ChargeKind::Repo, _) => c.repo.clone().map_or(Place::Unlinked, Place::Repo),
        (ChargeKind::Epic, Some((repo, number))) => Place::Epic { repo, number },
        (ChargeKind::Issue, Some((repo, issue))) => {
            let prs = gh.prs(&repo, issue);
            let only = match prs {
                [only] => Some(only.number),
                _ => None,
            };
            Place::Issue {
                pr: linked_pr(prs, links, &repo).or(only),
                repo,
                issue,
            }
        }
        (ChargeKind::Pr, Some((repo, number))) => match gh.closed_by(&repo, number) {
            Some(issue) => Place::Issue {
                repo,
                issue,
                pr: Some(number),
            },
            None => Place::Pr { repo, number },
        },
        // A numbered kind with no number: the parser never makes one.
        _ => Place::Unlinked,
    }
}

/// A PR a session's transcript linked (`github_links`).
struct PrLink {
    url: String,
    number: Option<i64>,
    /// `owner/name`.
    repository: Option<String>,
}

/// A session's `github_links`, newest first.
const LINKS_SQL: &str = "SELECT pr_url, pr_number, pr_repository FROM github_links
     WHERE session_id = ?1 ORDER BY id DESC";

/// The `github_links` of `sessions` (newest first), each session's newest
/// first.
fn row_links(conn: &Connection, sessions: Vec<&str>) -> Result<Vec<PrLink>> {
    let mut stmt = conn.prepare_cached(LINKS_SQL)?;
    let mut out = Vec::new();
    for session in sessions {
        for link in stmt.query_map([session], |r| {
            Ok(PrLink {
                url: r.get(0)?,
                number: r.get(1)?,
                repository: r.get(2)?,
            })
        })? {
            out.push(link?);
        }
    }
    Ok(out)
}

/// The newest of the row's links that names one of `prs`, by URL or by
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

/// A session as its Session row shows it.
pub(crate) struct Session {
    /// Its process root, else its id: what its `/clear` chain shares.
    chain: String,
    /// Its orchestrator's name, else its registry name, else its id.
    pub(crate) label: String,
    /// Its first record.
    pub(crate) start: Option<i64>,
    /// Its first record to its last activity: the latest record its
    /// timeline counts.
    pub(crate) span: Option<(i64, i64)>,
}

/// A session's chain, start, last activity (the latest record its timeline
/// counts, read from the `events_session_time` index alone: v10, #46) and
/// names.
const SESSION_SQL: &str = "SELECT
         (SELECT ifnull(process_root, session_id) FROM sessions WHERE session_id = ?1),
         (SELECT started_at FROM sessions WHERE session_id = ?1),
         (SELECT max(timestamp) FROM events
          WHERE session_id = ?1 AND timestamp IS NOT NULL
            AND ifnull(type, '') NOT IN ('custom-title', 'ai-title', 'cost-state')),
         (SELECT name FROM agents WHERE session_id = ?1 AND agent_id IS NULL),
         (SELECT name FROM registry WHERE session_id = ?1 AND name <> ''
          ORDER BY gone, last_seen_ms DESC, id DESC LIMIT 1)";

impl Session {
    fn load(conn: &Connection, session_id: &str) -> Result<Self> {
        type Read = (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let (chain, start, last, name, listed): Read = conn
            .prepare_cached(SESSION_SQL)?
            .query_row([session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?;
        let start = start.as_deref().and_then(parse_timestamp);
        let last = last.as_deref().and_then(parse_timestamp);
        Ok(Self {
            chain: chain.unwrap_or_else(|| session_id.to_string()),
            label: name.or(listed).unwrap_or_else(|| session_id.to_string()),
            start,
            span: start.map(|s| (s, last.unwrap_or(s).max(s))),
        })
    }
}

/// [`Agent::load_all`]'s query: each subagent's first and last record from
/// the `events_timeline` index.
const AGENTS_SQL: &str = "SELECT a.session_id, a.agent_id,
     coalesce(a.name, a.description, a.agent_type), a.agent_type, a.run_ms,
     (SELECT min(e.timestamp) FROM events AS e INDEXED BY events_timeline
      WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id
        AND e.timestamp IS NOT NULL),
     (SELECT max(e.timestamp) FROM events AS e INDEXED BY events_timeline
      WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id
        AND e.timestamp IS NOT NULL)
 FROM agents AS a
 WHERE a.agent_id IS NOT NULL AND a.session_id IN (
     SELECT session_id FROM audit_cells
     WHERE day BETWEEN ?1 AND ?2 AND agent_id IS NOT NULL
 )";

/// A subagent as its node shows it.
pub(crate) struct Agent {
    pub(crate) label: String,
    pub(crate) agent_type: Option<String>,
    pub(crate) run_ms: Option<i64>,
    /// Its first record to its last.
    pub(crate) span: Option<(i64, i64)>,
}

impl Agent {
    /// Every subagent of the sessions with subagent cells on days
    /// `from..=to`, by (session, agent id), in one query; each one's first
    /// and last record from the `events_timeline` index.
    fn load_all(
        conn: &Connection,
        from: &str,
        to: &str,
    ) -> Result<HashMap<(String, String), Self>> {
        conn.prepare_cached(AGENTS_SQL)?
            .query_map([from, to], |r| {
                let agent_id: String = r.get(1)?;
                let at = |i: usize| -> Result<Option<i64>> {
                    Ok(r.get::<_, Option<String>>(i)?
                        .as_deref()
                        .and_then(parse_timestamp))
                };
                Ok((
                    (r.get(0)?, agent_id.clone()),
                    Self {
                        label: r.get::<_, Option<String>>(2)?.unwrap_or(agent_id),
                        agent_type: r.get(3)?,
                        run_ms: r.get(4)?,
                        span: at(5)?.zip(at(6)?),
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
            span: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::query_plan;
    use crate::tail::test_dir;

    /// `events` rows carry their whole record, so a scan of a session's
    /// rows costs ~1 ms each: its last activity comes from the v10 index
    /// alone instead (#46).
    #[test]
    fn a_sessions_last_activity_reads_its_index_alone() {
        let dir = test_dir("audit-plans");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        let session = query_plan(&conn, SESSION_SQL).join("\n");
        assert!(
            session.contains("COVERING INDEX events_session_time"),
            "{session}"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    /// A subagent's first and last record come from the timeline index
    /// alone: the performance read loads every subagent in the store.
    #[test]
    fn a_subagents_run_reads_the_timeline_index() {
        let dir = test_dir("agents-plan");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        let plan = query_plan(&conn, AGENTS_SQL).join("\n");
        let by_index =
            "SEARCH e USING COVERING INDEX events_timeline (session_id=? AND agent_id=?)";
        assert_eq!(plan.matches(by_index).count(), 2, "{plan}");
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    /// A session's links are read by its key: no scan of `github_links`
    /// for every Session row (#67).
    #[test]
    fn a_sessions_links_are_read_by_index() {
        let dir = test_dir("links-plan");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        let plan = query_plan(&conn, LINKS_SQL).join("\n");
        assert!(
            plan.starts_with("SEARCH github_links USING INDEX"),
            "{plan}"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
