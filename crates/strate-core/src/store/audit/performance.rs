//! The Performance read (#51): how well the harness works, over the cells
//! and GitHub cache [`super::audit`] reads, under the same filter (local
//! days, repos).
//!
//! - Outcome: per local day of the range, every day, the issues closed as
//!   completed and the PRs merged on it, each at its whole-life $ and
//!   orchestrator active time (all its `<repo>-<issue>` work, in the range
//!   or not), plus the range's spend with no outcome yet.
//! - Agents: per agent type and model, each dispatch with a cell in the
//!   range once, by outcome; orchestrator rows per model, flagged.
//! - Bottleneck: per local day and repo, your own time in the loop, a
//!   blocked proxy (a blocked dispatch's last record to its parent's next
//!   typed prompt or Agent/SendMessage dispatch, on the day it went quiet),
//!   and the permission waits the hooks recorded (#52), on the day each
//!   started, in the window its agent was in.
//! - Waste: rework and errors in the range.
//!
//! A dispatch with no notification is running while its last record is
//! within 30 minutes of the store's newest record, so a read is the same
//! whenever it runs.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::ToSpan;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rusqlite::{Connection, params};
use serde::Serialize;

use super::cells::{self, Cell, Cells, Place};
use super::gh::Gh;
use super::{AuditFilter, Tokens};
use crate::names::parse_name;
use crate::store::{Result, account};
use crate::time::{USER_TOOLS, local_day, parse_timestamp};

/// A dispatch never notified is running while its last record is this
/// close to the store's newest.
const RUNNING_MS: i64 = 30 * 60 * 1000;

/// A context peak at or over this % of its model's window is a blowup.
const PEAK_PCT: f64 = 80.0;

/// Every day a cell can have, for an issue's whole life.
const ALL_DAYS: (&str, &str) = ("0001-01-01", "9999-12-31");

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Performance {
    pub outcome: Outcome,
    /// Orchestrator rows first, then by $ (most first).
    pub agents: Vec<AgentRow>,
    pub bottleneck: Bottleneck,
    pub waste: Waste,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// Every local day of the range, in order, the quiet ones too.
    pub days: Vec<OutcomeDay>,
    /// The range's $ in windows with no `<repo>-<issue>` name.
    pub unlinked_usd: f64,
    pub unlinked_workstreams: i64,
    /// The range's $ on issues not closed (or not in the GitHub cache).
    pub open_usd: f64,
    pub open_issues: i64,
}

/// One local day: the issues closed as completed and the PRs merged on it,
/// each at its whole-life cost.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeDay {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub issues_closed: i64,
    pub usd_per_closed_issue: Option<f64>,
    /// Orchestrator active time, not subagent time.
    pub active_ms_per_closed_issue: Option<i64>,
    /// With linked work; a PR closing several issues counts once.
    pub prs_merged: i64,
    pub usd_per_merged_pr: Option<f64>,
    /// Everything spent on it.
    pub spend_usd: f64,
}

/// One agent type on one model, or the orchestrator on one model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    /// `orchestrator` on an orchestrator row; `""` when unknown.
    pub agent_type: String,
    /// `""` when unknown.
    pub model: String,
    /// An orchestrator row: no dispatches, so every count is 0.
    pub orchestrator: bool,
    /// Each dispatch with a cell in the range, once.
    pub dispatches: i64,
    /// Completed with no `NEEDS_CONTEXT:` line in the final report.
    pub completed: i64,
    pub blocked: i64,
    /// Failed or killed.
    pub failed: i64,
    /// Never notified, and quiet for over 30 minutes before the store's
    /// newest record.
    pub no_report: i64,
    /// Never notified, last heard within those 30 minutes: no outcome yet.
    pub running: i64,
    /// SendMessage calls to these dispatches.
    pub followups: i64,
    /// Priced requests in the range.
    pub cost_usd: f64,
    pub usd_per_dispatch: Option<f64>,
    pub tokens: Tokens,
    /// Cache reads over all input: fresh, read and written.
    pub cache_hit_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bottleneck {
    /// By day, then repo; Unlinked last.
    pub days: Vec<BottleneckDay>,
    pub permission_waits: PermissionWaits,
}

/// Your time on one local day in one repo's windows (`None`: Unlinked).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BottleneckDay {
    pub day: String,
    pub repo: Option<String>,
    /// Answering questions and plan approvals.
    pub ask_ms: i64,
    /// From the last reply to your next prompt.
    pub prompt_wait_ms: i64,
    /// The blocked proxy, on the day the blocked dispatch went quiet.
    pub blocked_ms: i64,
    /// The orchestrator's active time.
    pub active_ms: i64,
    pub blocked_reports: i64,
    /// Permission prompts that started this day: answered ones' waits.
    pub permission_ms: i64,
    /// Those prompts, open ones included.
    pub permission_prompts: i64,
}

/// What the hooks have recorded of permission prompts (#52).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionWaits {
    /// The local day of the first recorded prompt in the store, whatever
    /// the filter; `None`: none recorded, so the hooks have never run.
    pub recorded_since: Option<String>,
    /// Prompts in the range and filter still unanswered: counted in
    /// `permission_prompts`, not in `permission_ms`.
    pub open: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waste {
    /// SendMessage calls to a dispatch, of `send_messages` in all.
    pub followups: i64,
    pub send_messages: i64,
    /// Of `dispatches`.
    pub blocked: i64,
    pub dispatches: i64,
    pub no_report: i64,
    /// The agent type and model with the most no-report dispatches.
    pub no_report_top: Option<LaneCount>,
    /// `compact_boundary` records.
    pub compactions: i64,
    /// `/clear` successors started in the range.
    pub clears: i64,
    /// Agents, orchestrators too, whose deepest request reached 80% of
    /// their model's window.
    pub context_peaks_over_80: i64,
    pub deepest_peak: Option<Peak>,
    /// `api_error` system records: a request retried.
    pub api_retries: i64,
    /// API error messages, prompt-too-long ones included.
    pub api_errors: i64,
    pub prompt_too_long: i64,
    /// Issues with work in the range that GitHub saw reopened.
    pub reopened_issues: i64,
    /// Merged revert PRs, merged in the range.
    pub reverted_prs: i64,
    /// The repos with GitHub data the two counts above cover.
    pub repos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneCount {
    pub agent_type: String,
    pub model: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Peak {
    pub pct: f64,
    /// `""` when unknown.
    pub model: String,
}

/// The performance read for `filter`. Fails on a day that is not
/// `YYYY-MM-DD`.
pub fn performance(conn: &Connection, filter: &AuditFilter) -> Result<Performance> {
    let range = Range::new(filter)?;
    let gh = Gh::load(conn)?;
    let whole_life = AuditFilter {
        from: ALL_DAYS.0.into(),
        to: ALL_DAYS.1.into(),
        ..filter.clone()
    };
    let cells = Cells::load(conn, &gh, &whole_life)?;
    let in_range: Vec<&Cell> = cells.list.iter().filter(|c| range.holds(&c.day)).collect();
    let read = Read {
        conn,
        filter,
        range,
        tz: account::zone(conn)?,
        gh,
        agents: AgentInfo::load_all(conn)?,
        newest: newest_record(conn)?,
    };
    let places = Places::load(conn)?;
    let dispatches = read.dispatches(&cells, &in_range);
    let agents = read.agent_rows(&in_range, &dispatches);
    Ok(Performance {
        outcome: read.outcome(&cells, &in_range),
        bottleneck: read.bottleneck(&cells, &in_range, &dispatches, &places)?,
        waste: read.waste(&cells, &in_range, &dispatches, &agents, &places)?,
        agents,
    })
}

/// The filter's days, checked.
struct Range {
    from: String,
    to: String,
    first: Date,
    last: Date,
}

impl Range {
    fn new(filter: &AuditFilter) -> Result<Self> {
        let (from, to) = (cells::day(&filter.from)?, cells::day(&filter.to)?);
        let date = |s: &str| s.parse::<Date>().expect("a checked day");
        Ok(Self {
            first: date(from),
            last: date(to),
            from: from.into(),
            to: to.into(),
        })
    }

    fn holds(&self, day: &str) -> bool {
        (self.from.as_str()..=self.to.as_str()).contains(&day)
    }

    /// Every calendar day of the range, in order: a civil date is a wall-clock
    /// day, so a DST day of 23 or 25 hours is still one.
    fn days(&self) -> impl Iterator<Item = Date> {
        let last = self.last;
        self.first.series(1.day()).take_while(move |d| *d <= last)
    }
}

/// The store's newest record time: the clock "running" is judged by.
fn newest_record(conn: &Connection) -> Result<Option<i64>> {
    let newest: Option<String> =
        conn.query_row("SELECT max(timestamp) FROM events", [], |r| r.get(0))?;
    Ok(newest.as_deref().and_then(parse_timestamp))
}

/// Agents by (session, agent id; `None`: the orchestrator).
type AgentKey = (String, Option<String>);

/// An `agents` row as this read needs it.
struct AgentInfo {
    agent_type: Option<String>,
    model: Option<String>,
    outcome: Option<String>,
    followups: i64,
    peak: Option<f64>,
    /// The window a subagent started in.
    segment: Option<i64>,
    /// A subagent's last record: as stored, and in ms.
    last: Option<(String, i64)>,
    /// The agent that dispatched it.
    parent: Option<AgentKey>,
}

impl AgentInfo {
    fn load_all(conn: &Connection) -> Result<HashMap<AgentKey, Self>> {
        let mut stmt = conn.prepare_cached(
            "SELECT a.session_id, a.agent_id, a.agent_type, a.model, a.outcome, a.followups,
                 a.context_peak_pct, a.segment_id,
                 CASE WHEN a.agent_id IS NOT NULL THEN
                     (SELECT max(e.timestamp) FROM events AS e
                      WHERE e.session_id = a.session_id AND e.agent_id = a.agent_id)
                 END,
                 p.session_id, p.agent_id
             FROM agents AS a
             LEFT JOIN edges AS d ON d.child = a.id AND d.kind = 'dispatch'
             LEFT JOIN agents AS p ON p.id = d.parent",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = HashMap::new();
        while let Some(r) = rows.next()? {
            let last: Option<String> = r.get(8)?;
            let parent: Option<String> = r.get(9)?;
            let info = Self {
                agent_type: r.get(2)?,
                model: r.get(3)?,
                outcome: r.get(4)?,
                followups: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                peak: r.get(6)?,
                segment: r.get(7)?,
                last: last.and_then(|s| parse_timestamp(&s).map(|ms| (s, ms))),
                parent: match parent {
                    Some(session) => Some((session, r.get(10)?)),
                    None => None,
                },
            };
            out.insert((r.get(0)?, r.get(1)?), info);
        }
        Ok(out)
    }
}

/// How a dispatch ended, as far as the store knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Completed,
    Blocked,
    Failed,
    NoReport,
    Running,
}

/// A subagent with a cell in the range.
struct Dispatch<'a> {
    key: AgentKey,
    info: Option<&'a AgentInfo>,
    agent_type: String,
    model: String,
    status: Status,
    /// Its window's repo.
    repo: Option<String>,
}

/// Each segment's repo, and each session's segments in transcript order:
/// where a stored event counts, as `account` places requests.
struct Places {
    repos: HashMap<i64, Option<String>>,
    /// (start offset, segment id, earliest record ms), by offset.
    sessions: HashMap<String, Vec<(i64, i64, Option<i64>)>>,
}

impl Places {
    fn load(conn: &Connection) -> Result<Self> {
        let mut out = Self {
            repos: HashMap::new(),
            sessions: HashMap::new(),
        };
        let mut stmt = conn.prepare_cached(
            "SELECT g.id, g.session_id, g.start_offset, w.name, w.fallback, g.started_at
             FROM segments AS g
             JOIN workstream_roots AS r ON r.id = g.workstream_id
             JOIN workstreams AS w ON w.id = r.root
             ORDER BY g.session_id, g.start_offset",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let name: String = r.get(3)?;
            let fallback: bool = r.get(4)?;
            let repo = (!fallback)
                .then(|| parse_name(&name))
                .flatten()
                .map(|p| p.repo);
            out.repos.insert(id, repo);
            let started: Option<String> = r.get(5)?;
            out.sessions.entry(r.get(1)?).or_default().push((
                r.get(2)?,
                id,
                started.as_deref().and_then(parse_timestamp),
            ));
        }
        Ok(out)
    }

    /// The repo of `segment` (`None`: Unlinked).
    fn repo(&self, segment: Option<i64>) -> Option<&str> {
        segment
            .and_then(|s| self.repos.get(&s))
            .and_then(Option::as_deref)
    }

    /// The window holding orchestrator byte `offset` of `session`.
    fn segment_at(&self, session: &str, offset: i64) -> Option<i64> {
        let segments = self.sessions.get(session)?;
        segments
            .iter()
            .rev()
            .find(|(start, _, _)| *start <= offset)
            .map(|(_, id, _)| *id)
    }

    /// The window of `session`'s orchestrator at `ms`: the last one whose
    /// earliest record is no later, else its first.
    fn segment_at_time(&self, session: &str, ms: i64) -> Option<i64> {
        let segments = self.sessions.get(session)?;
        segments
            .iter()
            .rev()
            .find(|(_, _, started)| started.is_some_and(|s| s <= ms))
            .or(segments.first())
            .map(|(_, id, _)| *id)
    }

    /// `session`'s first window.
    fn first_segment(&self, session: &str) -> Option<i64> {
        self.sessions.get(session)?.first().map(|(_, id, _)| *id)
    }
}

struct Read<'a> {
    conn: &'a Connection,
    filter: &'a AuditFilter,
    range: Range,
    tz: TimeZone,
    gh: Gh,
    agents: HashMap<AgentKey, AgentInfo>,
    newest: Option<i64>,
}

impl Read<'_> {
    /// Whether windows of `repo` (`None`: Unlinked, `""` in the filter)
    /// are in the filter.
    fn wanted(&self, repo: Option<&str>) -> bool {
        let slug = repo.unwrap_or("");
        self.filter.repos.is_empty() || self.filter.repos.iter().any(|w| w == slug)
    }

    /// The local day of `ms`, when the range holds it.
    fn day_in_range(&self, ms: i64) -> Option<String> {
        local_day(&self.tz, ms).filter(|d| self.range.holds(d))
    }

    fn outcome(&self, cells: &Cells, in_range: &[&Cell]) -> Outcome {
        // Whole life: per issue its $ and orchestrator active ms; per PR
        // (its own repo) its $ and merge time.
        let mut issues: HashMap<(&str, i64), (f64, i64)> = HashMap::new();
        let mut prs: HashMap<(&str, i64), (f64, Option<i64>)> = HashMap::new();
        for cell in &cells.list {
            let Place::Linked { repo, issue, pr } = &cells.window(cell).place else {
                continue;
            };
            let usd = cell.share.cost_usd.unwrap_or(0.0);
            let life = issues.entry((repo, *issue)).or_default();
            life.0 += usd;
            life.1 += cell.share.line.active_ms;
            if let Some(pr) = pr.and_then(|n| self.gh.pr(repo, *issue, n)) {
                prs.entry((&pr.repo, pr.number))
                    .or_insert((0.0, pr.merged_at))
                    .0 += usd;
            }
        }
        let mut days: BTreeMap<Date, Day> = BTreeMap::new();
        for (&(repo, number), &life) in &issues {
            let closed = self
                .gh
                .issue(repo, number)
                .filter(|i| i.completed)
                .and_then(|i| i.closed.flatten());
            if let Some(d) = day(&mut days, closed.and_then(|at| self.day_in_range(at))) {
                d.closed.push(life);
            }
        }
        for &(usd, merged_at) in prs.values() {
            if let Some(d) = day(&mut days, merged_at.and_then(|at| self.day_in_range(at))) {
                d.merged.push(usd);
            }
        }
        let (mut unlinked_usd, mut unlinked) = (0.0, HashSet::new());
        let (mut open_usd, mut open) = (0.0, HashSet::new());
        for cell in in_range {
            let usd = cell.share.cost_usd.unwrap_or(0.0);
            if let Some(d) = day(&mut days, Some(cell.day.clone())) {
                d.spend += usd;
            }
            let window = cells.window(cell);
            match &window.place {
                Place::Unlinked => {
                    unlinked_usd += usd;
                    unlinked.insert(window.label.as_str());
                }
                Place::Linked { repo, issue, .. }
                    if self
                        .gh
                        .issue(repo, *issue)
                        .is_none_or(|i| i.closed.is_none()) =>
                {
                    open_usd += usd;
                    open.insert((repo.as_str(), *issue));
                }
                Place::Linked { .. } => {}
            }
        }
        Outcome {
            days: self
                .range
                .days()
                .map(|d| days.remove(&d).unwrap_or_default().row(d))
                .collect(),
            unlinked_usd,
            unlinked_workstreams: unlinked.len() as i64,
            open_usd,
            open_issues: open.len() as i64,
        }
    }

    /// Each subagent with a cell in the range, once, in cell order.
    fn dispatches(&self, cells: &Cells, in_range: &[&Cell]) -> Vec<Dispatch<'_>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cell in in_range {
            let Some(agent_id) = &cell.agent_id else {
                continue;
            };
            let key = (cell.session_id.clone(), Some(agent_id.clone()));
            if !seen.insert(key.clone()) {
                continue;
            }
            let info = self.agents.get(&key);
            out.push(Dispatch {
                agent_type: info.and_then(|i| i.agent_type.clone()).unwrap_or_default(),
                model: info
                    .and_then(|i| i.model.clone())
                    .or_else(|| cell.model.clone())
                    .unwrap_or_default(),
                status: info.map_or(Status::NoReport, |i| self.status(i)),
                repo: cells.window(cell).repo.clone(),
                info,
                key,
            });
        }
        out
    }

    fn status(&self, info: &AgentInfo) -> Status {
        match info.outcome.as_deref() {
            Some("completed") => Status::Completed,
            Some("blocked") => Status::Blocked,
            Some(_) => Status::Failed,
            None => match (self.newest, &info.last) {
                (Some(newest), Some((_, last))) if newest - last <= RUNNING_MS => Status::Running,
                _ => Status::NoReport,
            },
        }
    }

    fn agent_rows(&self, in_range: &[&Cell], dispatches: &[Dispatch]) -> Vec<AgentRow> {
        let lanes: HashMap<&AgentKey, &Dispatch> = dispatches.iter().map(|d| (&d.key, d)).collect();
        let mut rows: HashMap<(bool, String, String), AgentRow> = HashMap::new();
        for cell in in_range {
            let lane = match &cell.agent_id {
                // An orchestrator cell with no model carries no request.
                None => match &cell.model {
                    Some(model) => (true, "orchestrator".to_string(), model.clone()),
                    None => continue,
                },
                Some(agent_id) => {
                    let key = (cell.session_id.clone(), Some(agent_id.clone()));
                    let Some(d) = lanes.get(&key) else { continue };
                    (false, d.agent_type.clone(), d.model.clone())
                }
            };
            let row = rows
                .entry(lane.clone())
                .or_insert_with(|| AgentRow::new(lane));
            row.cost_usd += cell.share.cost_usd.unwrap_or(0.0);
            row.tokens.add(&cell.share.tokens);
        }
        for d in dispatches {
            let lane = (false, d.agent_type.clone(), d.model.clone());
            let row = rows
                .entry(lane.clone())
                .or_insert_with(|| AgentRow::new(lane));
            row.dispatches += 1;
            row.followups += d.info.map_or(0, |i| i.followups);
            *match d.status {
                Status::Completed => &mut row.completed,
                Status::Blocked => &mut row.blocked,
                Status::Failed => &mut row.failed,
                Status::NoReport => &mut row.no_report,
                Status::Running => &mut row.running,
            } += 1;
        }
        let mut rows: Vec<AgentRow> = rows.into_values().map(AgentRow::finish).collect();
        rows.sort_by(|a, b| {
            b.orchestrator
                .cmp(&a.orchestrator)
                .then(b.cost_usd.total_cmp(&a.cost_usd))
                .then_with(|| (&a.agent_type, &a.model).cmp(&(&b.agent_type, &b.model)))
        });
        rows
    }

    fn bottleneck(
        &self,
        cells: &Cells,
        in_range: &[&Cell],
        dispatches: &[Dispatch],
        places: &Places,
    ) -> Result<Bottleneck> {
        let mut days: BTreeMap<DayKey, BottleneckDay> = BTreeMap::new();
        for cell in in_range.iter().filter(|c| c.agent_id.is_none()) {
            let t = cell.share.line;
            if t.active_ms == 0 && t.wait_ms == 0 {
                continue;
            }
            let row = day_row(&mut days, &cell.day, cells.window(cell).repo.clone());
            row.ask_ms += t.ask_ms;
            row.prompt_wait_ms += t.wait_ms - t.ask_ms;
            row.active_ms += t.active_ms;
        }
        for d in dispatches.iter().filter(|d| d.status == Status::Blocked) {
            let Some((info, (stored, last))) = d.info.and_then(|i| Some((i, i.last.as_ref()?)))
            else {
                continue;
            };
            let Some(day) = self.day_in_range(*last) else {
                continue;
            };
            let answered = match &info.parent {
                Some(parent) => self.next_answer(parent, stored)?,
                None => None,
            };
            let row = day_row(&mut days, &day, d.repo.clone());
            row.blocked_reports += 1;
            row.blocked_ms += answered.map_or(0, |at| (at - last).max(0));
        }
        let permission_waits = self.permission_waits(&mut days, places)?;
        Ok(Bottleneck {
            days: days.into_values().collect(),
            permission_waits,
        })
    }

    /// Adds each recorded permission wait in the range and filter to its
    /// start day's row, in the window its agent was in: an orchestrator's
    /// by time, a subagent's where it started. An open wait (no answer
    /// stored yet) counts as a prompt with no time: its end is unknown, and
    /// the live needs-you view already shows a current one. A question or
    /// plan approval is left out: the time model counts it as ask time.
    fn permission_waits(
        &self,
        days: &mut BTreeMap<DayKey, BottleneckDay>,
        places: &Places,
    ) -> Result<PermissionWaits> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.session_id, a.agent_id, w.start_ms, w.end_ms, w.tool_name
             FROM permission_waits AS w JOIN agents AS a ON a.id = w.agent",
        )?;
        let mut rows = stmt.query([])?;
        let mut open = 0;
        while let Some(r) = rows.next()? {
            let session: String = r.get(0)?;
            let agent_id: Option<String> = r.get(1)?;
            let start: i64 = r.get(2)?;
            let tool: Option<String> = r.get(4)?;
            if tool.is_some_and(|t| USER_TOOLS.contains(&t.as_str())) {
                continue;
            }
            let Some(day) = self.day_in_range(start) else {
                continue;
            };
            let segment = match agent_id {
                None => places.segment_at_time(&session, start),
                agent => self.agents.get(&(session, agent)).and_then(|i| i.segment),
            };
            let repo = places.repo(segment);
            if !self.wanted(repo) {
                continue;
            }
            let row = day_row(days, &day, repo.map(str::to_string));
            row.permission_prompts += 1;
            match r.get::<_, Option<i64>>(3)? {
                Some(end) => row.permission_ms += end - start,
                None => open += 1,
            }
        }
        let first: Option<i64> =
            self.conn
                .query_row("SELECT min(start_ms) FROM permission_waits", [], |r| {
                    r.get(0)
                })?;
        Ok(PermissionWaits {
            recorded_since: first.and_then(|ms| local_day(&self.tz, ms)),
            open,
        })
    }

    /// When `parent` next acted on its own after `after` (a stored
    /// timestamp): a typed prompt, or an Agent or SendMessage dispatch.
    fn next_answer(&self, parent: &AgentKey, after: &str) -> Result<Option<i64>> {
        let at: Option<String> = self
            .conn
            .prepare_cached(
                "SELECT min(timestamp) FROM events
                 WHERE session_id = ?1 AND agent_id IS ?2 AND timestamp > ?3
                   AND (prompt = 1 OR EXISTS (
                       SELECT 1 FROM json_each(tool_uses) AS u
                       WHERE json_extract(u.value, '$[1]') IN ('Agent', 'SendMessage')))",
            )?
            .query_row(params![parent.0, parent.1, after], |r| r.get(0))?;
        Ok(at.as_deref().and_then(parse_timestamp))
    }

    fn waste(
        &self,
        cells: &Cells,
        in_range: &[&Cell],
        dispatches: &[Dispatch],
        rows: &[AgentRow],
        places: &Places,
    ) -> Result<Waste> {
        let counts = self.event_counts(places)?;
        let count = |status: Status| dispatches.iter().filter(|d| d.status == status).count();
        let no_report_top = rows
            .iter()
            .filter(|r| r.no_report > 0)
            .min_by_key(|r| Reverse(r.no_report))
            .map(|r| LaneCount {
                agent_type: r.agent_type.clone(),
                model: r.model.clone(),
                count: r.no_report,
            });
        // Peaks: the dispatches and the orchestrators with cells in range.
        let orchestrators: HashSet<AgentKey> = in_range
            .iter()
            .filter(|c| c.agent_id.is_none())
            .map(|c| (c.session_id.clone(), None))
            .collect();
        let peaks: Vec<(f64, &str)> = orchestrators
            .iter()
            .filter_map(|k| self.agents.get(k))
            .chain(dispatches.iter().filter_map(|d| d.info))
            .filter_map(|i| Some((i.peak?, i.model.as_deref().unwrap_or(""))))
            .collect();
        let reopened: HashSet<(&str, i64)> = in_range
            .iter()
            .filter_map(|c| match &cells.window(c).place {
                Place::Linked { repo, issue, .. } => Some((repo.as_str(), *issue)),
                Place::Unlinked => None,
            })
            .filter(|&(repo, n)| {
                self.gh
                    .issue(repo, n)
                    .is_some_and(|i| i.reopened.is_some_and(|r| r > 0))
            })
            .collect();
        Ok(Waste {
            followups: dispatches
                .iter()
                .map(|d| d.info.map_or(0, |i| i.followups))
                .sum(),
            send_messages: counts.send_messages,
            blocked: count(Status::Blocked) as i64,
            dispatches: dispatches.len() as i64,
            no_report: count(Status::NoReport) as i64,
            no_report_top,
            compactions: counts.compactions,
            clears: self.clears(places)?,
            context_peaks_over_80: peaks.iter().filter(|(p, _)| *p >= PEAK_PCT).count() as i64,
            deepest_peak: peaks
                .iter()
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|&(pct, model)| Peak {
                    pct,
                    model: model.to_string(),
                }),
            api_retries: counts.api_retries,
            api_errors: counts.api_errors,
            prompt_too_long: counts.prompt_too_long,
            reopened_issues: reopened.len() as i64,
            reverted_prs: self.reverts()?,
            repos: self.gh_repos()?,
        })
    }

    /// The flagged events in the range and filter, placed in their window
    /// as their requests are: an orchestrator's by transcript offset, a
    /// subagent's in the window it started in.
    fn event_counts(&self, places: &Places) -> Result<EventCounts> {
        let mut out = EventCounts::default();
        let mut stmt = self.conn.prepare_cached(
            "SELECT session_id, agent_id, byte_offset, timestamp, type, subtype,
                 json_array_length(message_to), api_error
             FROM events
             WHERE message_to IS NOT NULL OR api_error IS NOT NULL
                 OR subtype IN ('compact_boundary', 'api_error')",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let session: String = r.get(0)?;
            let at: Option<String> = r.get(3)?;
            let segment = match r.get::<_, Option<String>>(1)? {
                None => places.segment_at(&session, r.get(2)?),
                agent => self.agents.get(&(session, agent)).and_then(|i| i.segment),
            };
            let in_scope = at
                .as_deref()
                .and_then(parse_timestamp)
                .and_then(|ms| self.day_in_range(ms))
                .is_some()
                && self.wanted(places.repo(segment));
            if !in_scope {
                continue;
            }
            let system = r.get::<_, Option<String>>(4)?.as_deref() == Some("system");
            match r.get::<_, Option<String>>(5)?.as_deref() {
                Some("compact_boundary") if system => out.compactions += 1,
                Some("api_error") if system => out.api_retries += 1,
                _ => {}
            }
            out.send_messages += r.get::<_, Option<i64>>(6)?.unwrap_or(0);
            match r.get::<_, Option<String>>(7)?.as_deref() {
                Some("prompt_too_long") => {
                    out.api_errors += 1;
                    out.prompt_too_long += 1;
                }
                Some(_) => out.api_errors += 1,
                None => {}
            }
        }
        Ok(out)
    }

    /// `/clear` successors (continuation children) that started in the
    /// range, in a window of the filter.
    fn clears(&self, places: &Places) -> Result<i64> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.session_id, s.started_at FROM edges AS e
             JOIN agents AS a ON a.id = e.child
             JOIN sessions AS s ON s.session_id = a.session_id
             WHERE e.kind = 'continuation'",
        )?;
        let mut rows = stmt.query([])?;
        let mut n = 0;
        while let Some(r) = rows.next()? {
            let session: String = r.get(0)?;
            let started: Option<String> = r.get(1)?;
            let in_range = started
                .as_deref()
                .and_then(parse_timestamp)
                .and_then(|ms| self.day_in_range(ms))
                .is_some();
            if in_range && self.wanted(places.repo(places.first_segment(&session))) {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Merged revert PRs of the filter's repos, merged in the range.
    fn reverts(&self) -> Result<i64> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT repo, merged_at FROM gh_reverts")?;
        let mut rows = stmt.query([])?;
        let mut n = 0;
        while let Some(r) = rows.next()? {
            let repo: String = r.get(0)?;
            let merged: String = r.get(1)?;
            let in_range = parse_timestamp(&merged)
                .and_then(|ms| self.day_in_range(ms))
                .is_some();
            if in_range && self.wanted(Some(&repo)) {
                n += 1;
            }
        }
        Ok(n)
    }

    /// The filter's repos with a successful GitHub refresh.
    fn gh_repos(&self) -> Result<i64> {
        let slugs: Vec<String> = self
            .conn
            .prepare_cached("SELECT slug FROM gh_repos WHERE fetched_at IS NOT NULL")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_>>()?;
        Ok(slugs.iter().filter(|s| self.wanted(Some(s))).count() as i64)
    }
}

/// The entry of local day `day` (`YYYY-MM-DD`).
fn day(days: &mut BTreeMap<Date, Day>, day: Option<String>) -> Option<&mut Day> {
    Some(days.entry(day?.parse::<Date>().ok()?).or_default())
}

/// A bottleneck row's key: by day, then repo with Unlinked last.
type DayKey = (String, bool, Option<String>);

fn day_row<'a>(
    days: &'a mut BTreeMap<DayKey, BottleneckDay>,
    day: &str,
    repo: Option<String>,
) -> &'a mut BottleneckDay {
    days.entry((day.to_string(), repo.is_none(), repo.clone()))
        .or_insert_with(|| BottleneckDay {
            day: day.to_string(),
            repo,
            ask_ms: 0,
            prompt_wait_ms: 0,
            blocked_ms: 0,
            active_ms: 0,
            blocked_reports: 0,
            permission_ms: 0,
            permission_prompts: 0,
        })
}

/// One day's closed issues (whole-life $, active ms), merged PRs' $ and
/// spend.
#[derive(Default)]
struct Day {
    closed: Vec<(f64, i64)>,
    merged: Vec<f64>,
    spend: f64,
}

impl Day {
    fn row(self, date: Date) -> OutcomeDay {
        let mean = |xs: &[f64]| (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64);
        let usd: Vec<f64> = self.closed.iter().map(|c| c.0).collect();
        let n = self.closed.len() as i64;
        let active: i64 = self.closed.iter().map(|c| c.1).sum();
        OutcomeDay {
            day: date.to_string(),
            issues_closed: n,
            usd_per_closed_issue: mean(&usd),
            active_ms_per_closed_issue: (n > 0).then(|| active / n),
            prs_merged: self.merged.len() as i64,
            usd_per_merged_pr: mean(&self.merged),
            spend_usd: self.spend,
        }
    }
}

#[derive(Default)]
struct EventCounts {
    send_messages: i64,
    compactions: i64,
    api_retries: i64,
    api_errors: i64,
    prompt_too_long: i64,
}

impl AgentRow {
    fn new((orchestrator, agent_type, model): (bool, String, String)) -> Self {
        Self {
            agent_type,
            model,
            orchestrator,
            dispatches: 0,
            completed: 0,
            blocked: 0,
            failed: 0,
            no_report: 0,
            running: 0,
            followups: 0,
            cost_usd: 0.0,
            usd_per_dispatch: None,
            tokens: Tokens::default(),
            cache_hit_pct: None,
        }
    }

    fn finish(mut self) -> Self {
        let t = &self.tokens;
        let input = t.input + t.cache_read + t.cache_write;
        self.cache_hit_pct = (input > 0).then(|| t.cache_read as f64 * 100.0 / input as f64);
        self.usd_per_dispatch =
            (self.dispatches > 0).then(|| self.cost_usd / self.dispatches as f64);
        self
    }
}
