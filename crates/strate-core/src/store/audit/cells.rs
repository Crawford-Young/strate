//! The `audit_cells` in a filter, with the `/rename` windows and subagents
//! they belong to, each resolved once.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row};

use super::gh::{Gh, GhPr};
use super::{AuditFilter, Tokens};
use crate::names::{ParsedName, is_director, parse_name};
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
    /// Its `segments` row (`None`: a session with no window, or a
    /// subagent whose start no window holds).
    pub(crate) segment: Option<i64>,
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
    pub(crate) rotations: i64,
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
                 ask_ms, cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
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
            if !filter.repos.is_empty() && !w.filed_under().is_some_and(wanted) {
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
                window,
                segment: row.segment_id,
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

    /// The Session rows `cell` sits under, outermost first, each shown by
    /// one of its windows: a unit's row nests under its director's, which
    /// may nest under its own director. The last is `cell`'s own window;
    /// the first places them all.
    pub(crate) fn rows(&self, cell: &Cell) -> Vec<&Window> {
        let mut rows: Vec<&Window> = self.windows.up(cell.window).collect();
        rows.reverse();
        rows
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
pub(crate) fn day(s: &str) -> Result<&str> {
    match s.parse::<jiff::civil::Date>() {
        Ok(d) if d.to_string() == s => Ok(s),
        _ => Err(rusqlite::Error::ToSqlConversionFailure(
            format!("not a YYYY-MM-DD day: {s:?}").into(),
        )),
    }
}

/// Where a window's work belongs: its Session row's place.
#[derive(Clone)]
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
    /// Its Session row: `<chain>/<workstream>`, the chain its session's
    /// process root (every `/clear` link of one process shares it) and the
    /// workstream its merge-resolved name, so a chain's windows in one
    /// workstream are one row; `session:<id>` for a session with no window.
    pub(crate) row: String,
    pub(crate) label: String,
    /// The repo its name parses to, linked or not.
    pub(crate) repo: Option<String>,
    /// Named `-sN`, any N: a rotation is a `-sN` window, not "windows - 1".
    rotation: bool,
    /// Where its name puts it.
    pub(crate) place: Place,
    /// A row whose name gives no place but whose units or `director-<repo>`
    /// name do (#63): the repo the tree puts it under, at repo level with
    /// no issue (see [`Windows::director_repo`]).
    pub(crate) director_repo: Option<String>,
    /// Its start to the next window's start, or to the session's last
    /// activity.
    pub(crate) span: Option<(i64, i64)>,
}

impl Window {
    /// The repo the repo filter reads it by: its name's, else the one its
    /// row is placed under.
    fn filed_under(&self) -> Option<&String> {
        self.repo.as_ref().or(self.director_repo.as_ref())
    }
}

/// Every window the filter's cells touch, and their directors', resolved
/// once.
#[derive(Default)]
struct Windows {
    list: Vec<Window>,
    index: HashMap<(String, Option<i64>), usize>,
    sessions: HashMap<String, Session>,
    /// The filter's segments by id, read in one query.
    named: HashMap<i64, Named>,
    /// Per Session row, the director's window it nests under (see
    /// [`launched_from`]); `None` while it resolves.
    directors: HashMap<String, Option<usize>>,
    /// Per Session row, its place (see [`Windows::row_place`]).
    places: HashMap<String, Place>,
    /// Per Session row whose name gives no place, its director repo (see
    /// [`Windows::director_repo`]).
    director_repos: HashMap<String, Option<String>>,
    /// The repos a `director-<repo>` name can name, read once when needed.
    known_repos: Option<HashSet<String>>,
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
        // A director's window may have no cells in the filter.
        let named = match segment.map(|id| (id, self.named.remove(&id))) {
            Some((_, Some(named))) => Some(named),
            Some((id, None)) => Named::load(conn, id)?,
            None => None,
        };
        // With no window, the session's id and start.
        let named = named.unwrap_or_else(|| Named {
            name: session_id.to_string(),
            fallback: true,
            start: session.start,
            next: None,
            workstream: None,
        });
        let parsed = if named.fallback {
            None
        } else {
            parse_name(&named.name)
        };
        let end = named.next.or(session.last);
        let unit = segment.and(named.workstream);
        let chain = session.chain.clone();
        let (row, place, director_repo) = match unit {
            Some(workstream) => {
                let row = format!("{chain}/{workstream}");
                let place = self.row_place(conn, gh, &row, parsed.as_ref(), &chain, workstream)?;
                let director_repo = match parsed {
                    Some(_) => None,
                    None => {
                        self.director_repo(conn, &chain, workstream, &named.name, &mut vec![])?
                    }
                };
                (row, place, director_repo)
            }
            // No window: a fallback name, so Unlinked.
            None => (format!("session:{session_id}"), Place::Unlinked, None),
        };
        self.list.push(Window {
            row: row.clone(),
            place,
            director_repo,
            repo: parsed.as_ref().map(|p| p.repo.clone()),
            rotation: parsed.is_some_and(|p| p.rotation.is_some()),
            label: named.name,
            span: named.start.map(|s| (s, end.unwrap_or(s).max(s))),
        });
        let at = self.list.len() - 1;
        self.index.insert(key, at);
        if let Some(workstream) = unit {
            self.resolve_director(conn, gh, &row, &chain, workstream)?;
        }
        Ok(at)
    }

    /// The place of Session row `row` (`chain`'s windows in `workstream`,
    /// all named `parsed`), read once per row: every window of a row sits
    /// in one place, so a row never splits into two nodes with one key. Its
    /// PR is the one the newest `github_links` entry of any of those
    /// sessions names (see [`row_links`]), whichever window's transcript
    /// holds the link.
    fn row_place(
        &mut self,
        conn: &Connection,
        gh: &Gh,
        row: &str,
        parsed: Option<&ParsedName>,
        chain: &str,
        workstream: i64,
    ) -> Result<Place> {
        if let Some(found) = self.places.get(row) {
            return Ok(found.clone());
        }
        let found = place(gh, parsed, &row_links(conn, chain, workstream)?);
        self.places.insert(row.to_string(), found.clone());
        Ok(found)
    }

    /// Where Session row `chain`'s windows in `workstream`, named `name`
    /// (which gives no place), sits (#63), read once per row: under the one
    /// repo all its units' rows (see [`unit_rows`]) that name one work in,
    /// each by its name, or for a unit whose name gives no place, where it
    /// sits in turn (a unit placed nowhere, such as one named by a pasted
    /// launch line, is skipped); with those in several repos, or none (a
    /// row with no units too), under the known repo a `director-<repo>`
    /// name names ([`Windows::named_repo`]). `None` when neither places it.
    /// `open` holds the rows being resolved, so rows that direct each other
    /// end the walk.
    fn director_repo(
        &mut self,
        conn: &Connection,
        chain: &str,
        workstream: i64,
        name: &str,
        open: &mut Vec<String>,
    ) -> Result<Option<String>> {
        let row = format!("{chain}/{workstream}");
        if let Some(found) = self.director_repos.get(&row) {
            return Ok(found.clone());
        }
        if open.contains(&row) {
            return Ok(None);
        }
        let units = unit_rows(conn, chain, workstream)?;
        open.push(row.clone());
        let mut repos = Vec::with_capacity(units.len());
        for unit in units {
            let repo = match unit.parsed {
                Some(p) => Some(p.repo),
                None => self.director_repo(conn, &unit.chain, unit.workstream, &unit.name, open)?,
            };
            repos.extend(repo);
        }
        open.pop();
        let found = match repos.split_first() {
            Some((first, rest)) if rest.iter().all(|r| r == first) => Some(first.clone()),
            _ => self.named_repo(conn, name)?,
        };
        self.director_repos.insert(row, found.clone());
        Ok(found)
    }

    /// The repo a `director-<repo>` name names, when the store knows it:
    /// a repo the GitHub cache holds or a window name parses to (in any
    /// case, as the cache or name spells it).
    fn named_repo(&mut self, conn: &Connection, name: &str) -> Result<Option<String>> {
        let Some((_, scope)) = name.split_once('-').filter(|_| is_director(name)) else {
            return Ok(None);
        };
        let known = match &mut self.known_repos {
            Some(known) => known,
            empty => empty.insert(known_repos(conn)?),
        };
        Ok(known
            .iter()
            .find(|r| r.eq_ignore_ascii_case(scope))
            .cloned())
    }

    /// Records the director's window Session row `row` (`chain`'s windows
    /// in `workstream`) nests under (see [`launched_from`]), once per row.
    /// The row is marked `None` while its director resolves, so a director
    /// chain that leads back to it ends there.
    fn resolve_director(
        &mut self,
        conn: &Connection,
        gh: &Gh,
        row: &str,
        chain: &str,
        workstream: i64,
    ) -> Result<()> {
        if self.directors.contains_key(row) {
            return Ok(());
        }
        self.directors.insert(row.to_string(), None);
        let director = match launched_from(conn, chain, workstream)? {
            Some((parent, segment)) => Some(self.get(conn, gh, &parent, segment)?),
            None => None,
        };
        // Rows that direct each other: the one resolved first stays on top.
        let director = director.filter(|&d| self.up(d).all(|w| w.row != row));
        self.directors.insert(row.to_string(), director);
        Ok(())
    }

    /// Window `at`, then the director's window its row nests under, and up.
    /// A row nests only under a director whose walk up never meets it, so
    /// the walk ends.
    fn up(&self, at: usize) -> impl Iterator<Item = &Window> {
        std::iter::successors(Some(&self.list[at]), |w| {
            self.directors
                .get(&w.row)
                .copied()
                .flatten()
                .map(|d| &self.list[d])
        })
    }
}

/// Where a unit's Session row (`chain`'s windows in `workstream`) was
/// launched from: of its sessions with a window there that a director
/// messaged first (`directs`), the earliest's director session, and the
/// window of the message it sent (`None`: no window). The director's own
/// record is in the window live at its offset, a subagent's in that
/// subagent's window. `None` when no director messaged one.
fn launched_from(
    conn: &Connection,
    chain: &str,
    workstream: i64,
) -> Result<Option<(String, Option<i64>)>> {
    conn.prepare_cached(
        "SELECT x.parent, CASE WHEN e.agent_id IS NULL THEN (
                 SELECT g.id FROM segments AS g
                 WHERE g.session_id = e.session_id AND g.start_offset <= e.byte_offset
                 ORDER BY g.start_offset DESC LIMIT 1
             ) ELSE (
                 SELECT a.segment_id FROM agents AS a
                 WHERE a.session_id = e.session_id AND a.agent_id = e.agent_id
             ) END
         FROM directs AS x
         JOIN sessions AS c ON c.session_id = x.child
         JOIN events AS e ON e.sent_msg_id = x.msg_id AND e.session_id = x.parent
         WHERE ifnull(c.process_root, c.session_id) = ?1
           AND EXISTS (SELECT 1 FROM segments AS g
               JOIN workstream_roots AS r ON r.id = g.workstream_id
               WHERE g.session_id = x.child AND r.root = ?2)
         ORDER BY c.started_at IS NULL, c.started_at, x.child, e.id
         LIMIT 1",
    )?
    .query_row(rusqlite::params![chain, workstream], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .optional()
}

/// A unit's Session row, as [`unit_rows`] reads it.
struct UnitRow {
    chain: String,
    workstream: i64,
    /// Its merge-resolved name.
    name: String,
    /// What that name says, unless it is a cwd/branch fallback.
    parsed: Option<ParsedName>,
}

/// The unit rows that nest under Session row `chain`'s windows in
/// `workstream`: each row (unit chain and workstream) of a session one of
/// `chain`'s sessions directed whose [`launched_from`] is a window of
/// that row, in chain and workstream order.
fn unit_rows(conn: &Connection, chain: &str, workstream: i64) -> Result<Vec<UnitRow>> {
    let candidates: Vec<(String, i64, String, bool)> = conn
        .prepare_cached(
            "SELECT DISTINCT ifnull(c.process_root, c.session_id), r.root, w.name, w.fallback
             FROM directs AS x
             JOIN sessions AS p ON p.session_id = x.parent
             JOIN sessions AS c ON c.session_id = x.child
             JOIN segments AS g ON g.session_id = x.child
             JOIN workstream_roots AS r ON r.id = g.workstream_id
             JOIN workstreams AS w ON w.id = r.root
             WHERE ifnull(p.process_root, p.session_id) = ?1
             ORDER BY 1, 2",
        )?
        .query_map([chain], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_>>()?;
    let mut window_row = conn.prepare_cached(
        "SELECT (SELECT ifnull(process_root, session_id) FROM sessions WHERE session_id = ?1),
             (SELECT r.root FROM segments AS g JOIN workstream_roots AS r ON r.id = g.workstream_id
              WHERE g.id = ?2)",
    )?;
    let mut out = Vec::new();
    for (unit_chain, unit_workstream, name, fallback) in candidates {
        if (unit_chain.as_str(), unit_workstream) == (chain, workstream) {
            continue;
        }
        let Some((parent, Some(segment))) = launched_from(conn, &unit_chain, unit_workstream)?
        else {
            continue;
        };
        let (parent_chain, parent_workstream): (Option<String>, Option<i64>) = window_row
            .query_row(rusqlite::params![parent, segment], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if parent_chain.as_deref() == Some(chain) && parent_workstream == Some(workstream) {
            out.push(UnitRow {
                chain: unit_chain,
                workstream: unit_workstream,
                parsed: (!fallback).then(|| parse_name(&name)).flatten(),
                name,
            });
        }
    }
    Ok(out)
}

/// Every repo a `director-<repo>` name can name: the GitHub cache's, and
/// those window names parse to.
fn known_repos(conn: &Connection) -> Result<HashSet<String>> {
    let mut known: HashSet<String> = conn
        .prepare_cached("SELECT slug FROM gh_repos")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let names: Vec<String> = conn
        .prepare_cached("SELECT name FROM workstreams WHERE fallback = 0")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    known.extend(names.iter().filter_map(|n| parse_name(n)).map(|p| p.repo));
    Ok(known)
}

/// What every window of one session shares.
struct Session {
    /// Its process root, else its id: what its `/clear` chain shares.
    chain: String,
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

/// A session's chain, its start and its last activity: the latest record
/// its timeline counts, read from the `events_session_time` index alone
/// (v10, #46).
const SESSION_SQL: &str = "SELECT
         (SELECT ifnull(process_root, session_id) FROM sessions WHERE session_id = ?1),
         (SELECT started_at FROM sessions WHERE session_id = ?1),
         (SELECT max(timestamp) FROM events
          WHERE session_id = ?1 AND timestamp IS NOT NULL
            AND ifnull(type, '') NOT IN ('custom-title', 'ai-title', 'cost-state'))";

/// The `github_links` of every session of `chain` with a window in
/// `workstream` (a Session row's sessions), newest first: the latest
/// session's first, each session's in link order, newest first.
fn row_links(conn: &Connection, chain: &str, workstream: i64) -> Result<Vec<PrLink>> {
    conn.prepare_cached(
        "SELECT l.pr_url, l.pr_number, l.pr_repository
         FROM github_links AS l JOIN sessions AS s ON s.session_id = l.session_id
         WHERE ifnull(s.process_root, s.session_id) = ?1
           AND EXISTS (SELECT 1 FROM segments AS g
               JOIN workstream_roots AS r ON r.id = g.workstream_id
               WHERE g.session_id = s.session_id AND r.root = ?2)
         ORDER BY s.started_at IS NULL, s.started_at DESC, s.session_id DESC, l.id DESC",
    )?
    .query_map(rusqlite::params![chain, workstream], |r| {
        Ok(PrLink {
            url: r.get(0)?,
            number: r.get(1)?,
            repository: r.get(2)?,
        })
    })?
    .collect()
}

impl Session {
    fn load(conn: &Connection, session_id: &str) -> Result<Self> {
        let (chain, start, last): (Option<String>, Option<String>, Option<String>) = conn
            .prepare_cached(SESSION_SQL)?
            .query_row([session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(Self {
            chain: chain.unwrap_or_else(|| session_id.to_string()),
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
    /// Its merge-resolved workstream; `None` for a session with no window.
    workstream: Option<i64>,
}

/// Each segment matching `{segments}` with its merge-resolved name and
/// workstream, and the start of its session's next segment.
const NAMED_SQL: &str = "SELECT g.id, w.name, w.fallback, g.started_at, g.next, r.root
     FROM (
         SELECT id, workstream_id, started_at,
             lead(started_at) OVER (PARTITION BY session_id ORDER BY start_offset) AS next
         FROM segments
         WHERE {segments}
     ) AS g
     JOIN workstream_roots AS r ON r.id = g.workstream_id
     JOIN workstreams AS w ON w.id = r.root";

impl Named {
    /// Every segment of the sessions with cells on days `from..=to`, by id,
    /// in one query (one pass over the recursive `workstream_roots`).
    fn load_all(conn: &Connection, from: &str, to: &str) -> Result<HashMap<i64, Self>> {
        let sql = NAMED_SQL.replace(
            "{segments}",
            "session_id IN (SELECT session_id FROM audit_cells WHERE day BETWEEN ?1 AND ?2)",
        );
        conn.prepare_cached(&sql)?
            .query_map([from, to], Self::read)?
            .collect()
    }

    /// Segment `id` alone: a director's window with no cells in the filter.
    fn load(conn: &Connection, id: i64) -> Result<Option<Self>> {
        let sql = NAMED_SQL.replace(
            "{segments}",
            "session_id = (SELECT session_id FROM segments WHERE id = ?1)",
        );
        conn.prepare_cached(&format!("{sql} WHERE g.id = ?1"))?
            .query_row([id], Self::read)
            .optional()
            .map(|found| found.map(|(_, named)| named))
    }

    fn read(r: &Row) -> Result<(i64, Self)> {
        let at = |s: Option<String>| s.as_deref().and_then(parse_timestamp);
        Ok((
            r.get(0)?,
            Self {
                name: r.get(1)?,
                fallback: r.get(2)?,
                start: at(r.get(3)?),
                next: at(r.get(4)?),
                workstream: r.get(5)?,
            },
        ))
    }
}

/// Where a window named `parsed` belongs: under its repo and issue, and
/// under the PR its row links (else the issue's only PR) when the GitHub
/// cache holds the issue. Only a name that doesn't parse is Unlinked.
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

/// A subagent as its node shows it.
pub(crate) struct Agent {
    pub(crate) label: String,
    pub(crate) agent_type: Option<String>,
    pub(crate) run_ms: Option<i64>,
}

impl Agent {
    /// Every subagent of the sessions with subagent cells on days
    /// `from..=to`, by (session, agent id), in one query.
    fn load_all(
        conn: &Connection,
        from: &str,
        to: &str,
    ) -> Result<HashMap<(String, String), Self>> {
        conn.prepare_cached(
            "SELECT session_id, agent_id,
                 coalesce(registry_name, name, description, agent_type), agent_type, run_ms
             FROM agents
             WHERE agent_id IS NOT NULL AND session_id IN (
                 SELECT session_id FROM audit_cells
                 WHERE day BETWEEN ?1 AND ?2 AND agent_id IS NOT NULL
             )",
        )?
        .query_map([from, to], |r| {
            let agent_id: String = r.get(1)?;
            Ok((
                (r.get(0)?, agent_id.clone()),
                Self {
                    label: r.get::<_, Option<String>>(2)?.unwrap_or(agent_id),
                    agent_type: r.get(3)?,
                    run_ms: r.get(4)?,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::tail::test_dir;

    /// Each step of `sql`'s query plan, one per line, every parameter "".
    fn plan(conn: &Connection, sql: &str) -> String {
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .expect("plan");
        let params = vec![""; stmt.parameter_count()];
        stmt.query_map(rusqlite::params_from_iter(params), |r| {
            r.get::<_, String>(3)
        })
        .expect("steps")
        .collect::<Result<Vec<_>>>()
        .expect("detail")
        .join("\n")
    }

    /// `events` rows carry their whole record, so a scan of a session's
    /// rows costs ~1 ms each: its last activity comes from the v10 index
    /// alone instead (#46).
    #[test]
    fn a_sessions_last_activity_reads_its_index_alone() {
        let dir = test_dir("audit-plans");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        let session = plan(&conn, SESSION_SQL);
        assert!(
            session.contains("COVERING INDEX events_session_time"),
            "{session}"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
