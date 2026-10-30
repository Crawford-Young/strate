//! Cost and time in the store (#14). [`fields`] reads what accounting needs
//! from each record as it is inserted. Rollups are a pure function of the
//! stored events, recomputed in the batch's transaction for every session
//! the batch touches, so re-ingest is idempotent and an incremental build
//! converges to a cold one. Every function runs inside the caller's
//! transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Once;

use jiff::tz::TimeZone;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::{Result, model};
use crate::cost::{self, SYNTHETIC, Usage, truthy};
use crate::time::{self, Cat, IDLE_GAP_MS, Record, Split, Timed};

/// The accounting columns of one event (see `0003_cost_time.sql`,
/// `0008_performance.sql` and `0009_directs.sql`).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Fields {
    pub request_key: Option<String>,
    pub model: Option<String>,
    pub usage: Option<String>,
    pub output_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub tool_uses: Option<String>,
    pub tool_results: Option<String>,
    pub prompt: Option<bool>,
    pub notification_task: Option<String>,
    pub notification_status: Option<String>,
    pub message_to: Option<String>,
    pub api_error: Option<String>,
    pub sent_msg_id: Option<String>,
    pub peer_msg_id: Option<String>,
    pub peer_name: Option<String>,
}

impl Fields {
    fn notified(text: Option<&str>) -> Self {
        let (task, status) = text.and_then(notification).unzip();
        Self {
            notification_task: task,
            notification_status: status,
            ..Self::default()
        }
    }
}

/// Reads one record's accounting fields, as audit-lib's `add` and
/// `timeRecord` read them, plus its task notification, SendMessage targets,
/// API error and the msg_id of a cross-session message it sent or received.
pub(super) fn fields(value: &Value) -> Fields {
    match value["type"].as_str() {
        Some("assistant") => assistant_fields(value),
        Some("user") => user_fields(value),
        Some("queue-operation") => Fields::notified(value["content"].as_str()),
        Some("attachment") if value["attachment"]["type"] == "queued_command" => {
            Fields::notified(value["attachment"]["prompt"].as_str())
        }
        _ => Fields::default(),
    }
}

fn blocks(value: &Value) -> &[Value] {
    value["message"]["content"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
}

fn assistant_fields(value: &Value) -> Fields {
    let message = &value["message"];
    let tool_uses = || blocks(value).iter().filter(|b| b["type"] == "tool_use");
    let request_key = [&value["requestId"], &message["id"], &value["uuid"]]
        .into_iter()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
        .map(str::to_string);
    let uses: Vec<Value> = tool_uses()
        .filter_map(|b| {
            let id = b["id"].as_str().filter(|s| !s.is_empty())?;
            Some(json!([id, b["name"].as_str()]))
        })
        .collect();
    let to: Vec<&str> = tool_uses()
        .filter(|b| b["name"] == "SendMessage")
        .filter_map(|b| b["input"]["to"].as_str().filter(|s| !s.is_empty()))
        .collect();
    let usage = &message["usage"];
    let model = message["model"]
        .as_str()
        .filter(|m| !m.is_empty() && *m != SYNTHETIC && usage.is_object());
    let tokens = Usage::from_json(usage);
    Fields {
        request_key,
        model: model.map(str::to_string),
        usage: model.map(|_| usage.to_string()),
        output_tokens: model.map(|_| tokens.output as i64),
        cost_usd: model.and_then(|m| cost::prices().price(&tokens, m)),
        tool_uses: (!uses.is_empty()).then(|| Value::from(uses).to_string()),
        message_to: (!to.is_empty()).then(|| json!(to).to_string()),
        api_error: truthy(&value["isApiErrorMessage"]).then(|| api_error(value).to_string()),
        ..Fields::default()
    }
}

/// An API error's kind, by its text: Claude Code reports an over-long
/// context as "Prompt is too long".
fn api_error(value: &Value) -> &'static str {
    let text = message_text(value).to_ascii_lowercase();
    if text.contains("prompt is too long") {
        "prompt_too_long"
    } else {
        "other"
    }
}

fn user_fields(value: &Value) -> Fields {
    let results: Vec<Option<&str>> = blocks(value)
        .iter()
        .filter(|b| b["type"] == "tool_result")
        .map(|b| b["tool_use_id"].as_str())
        .collect();
    let prompt = (results.is_empty() && !truthy(&value["isMeta"])).then(|| is_human(value));
    // A prompt nobody typed may be a task notification.
    let text = (prompt == Some(false)).then(|| message_text(value));
    let id = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let origin = &value["origin"];
    let peer = origin["kind"] == "peer";
    Fields {
        tool_results: (!results.is_empty()).then(|| json!(results).to_string()),
        prompt,
        sent_msg_id: id(&value["toolUseResult"]["msg_id"]),
        peer_msg_id: peer.then(|| id(&origin["msg_id"])).flatten(),
        peer_name: peer.then(|| id(&origin["name"])).flatten(),
        ..Fields::notified(text.as_deref())
    }
}

/// The `<task-id>` and `<status>` of a text that is a task notification
/// (starts with one); `None` for anything else, a notification quoted in
/// prose or one with no status.
fn notification(text: &str) -> Option<(String, String)> {
    let body = text.trim_start().strip_prefix("<task-notification>")?;
    let body = body.split("</task-notification>").next().unwrap_or(body);
    let tag = |name: &str| {
        let (_, rest) = body.split_once(&format!("<{name}>"))?;
        let (inner, _) = rest.split_once(&format!("</{name}>"))?;
        Some(inner.trim().to_string()).filter(|s| !s.is_empty())
    };
    Some((tag("task-id")?, tag("status")?))
}

/// A record's message text: its string content, or its text parts joined.
fn message_text(value: &Value) -> String {
    match &value["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().or_else(|| p["text"].as_str()).unwrap_or(""))
            .collect(),
        _ => String::new(),
    }
}

/// A typed prompt, not a background agent's task notification: by
/// `origin.kind` when the record has one, else by its text.
fn is_human(value: &Value) -> bool {
    let kind = &value["origin"]["kind"];
    if truthy(kind) {
        return kind == "human";
    }
    !message_text(value)
        .trim_start()
        .starts_with("<task-notification")
}

/// The sessions a batch of `session_id` can change: its own, and any other
/// holding a record of one of the batch's requests (`keys`), whose share of
/// that request may have moved.
pub(super) fn touched(
    conn: &Connection,
    session_id: &str,
    keys: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::from([session_id.to_string()]);
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT session_id FROM events WHERE request_key = ?1 AND usage IS NOT NULL",
    )?;
    for key in keys {
        for sid in stmt.query_map([key], |r| r.get::<_, String>(0))? {
            out.insert(sid?);
        }
    }
    Ok(out)
}

/// Recomputes the rollups and audit cells of the given sessions and their
/// agents, with days in the zone under meta `tz`.
pub(super) fn rollup<'a>(conn: &Connection, ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let tz = zone(conn)?;
    for id in ids {
        session(conn, id, &tz)?;
    }
    Ok(())
}

/// The machine's IANA zone; UTC when it has none to name (a fixed offset
/// or a POSIX `TZ`), since meta `tz` stores a name. The fallback is logged
/// once per process.
pub(super) fn system_zone() -> String {
    static WARNED: Once = Once::new();
    TimeZone::try_system()
        .ok()
        .and_then(|tz| tz.iana_name().map(str::to_string))
        .unwrap_or_else(|| {
            WARNED.call_once(|| {
                eprintln!("strate: the system time zone has no IANA name; audit days are in UTC");
            });
            "UTC".to_string()
        })
}

/// The zone the stored days are in. Store::open writes meta `tz` before any
/// rollup, so the UTC fallback only covers a name this build cannot load.
pub(super) fn zone(conn: &Connection) -> Result<TimeZone> {
    Ok(meta(conn, "tz")?
        .and_then(|name| TimeZone::get(&name).ok())
        .unwrap_or(TimeZone::UTC))
}

fn meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.prepare_cached("SELECT value FROM meta WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )?
    .execute([key, value])?;
    Ok(())
}

/// What the stored fields and cells hold, under meta `rollup`. Bump it when
/// [`fields`] or the cells gain something a store accounted by an older
/// build lacks: the next open re-reads and recomputes everything once.
/// 2: `audit_cells.ask_ms`. 3: the notification, SendMessage and API-error
/// event columns and the agents' outcome, follow-ups and context peak (v8).
/// 4: the sent and peer msg_id event columns, and `directs` from them (v9).
/// 5: cells per (session, agent, day, model), no longer cut at `/rename`
/// windows (#107).
const ROLLUP_VERSION: &str = "5";

/// Recomputes every rollup and audit cell unless the store was last
/// accounted at the vendored prices, at [`cost::PRICING_LOGIC_VERSION`], in
/// `zone` (an IANA name) and at [`ROLLUP_VERSION`]. A new price list,
/// pricing-logic version or rollup version also re-reads every event's
/// accounting fields from `raw` and rebuilds the message edges from them. A store
/// from before v3 (no prices) or v6 (no zone) is filled here too.
pub(super) fn refresh(conn: &Connection, zone: &str) -> Result<()> {
    let repriced = meta(conn, "prices")?.as_deref() != Some(cost::PRICES_JSON)
        || meta(conn, "pricing_logic")?.as_deref() != Some(cost::PRICING_LOGIC_VERSION);
    let rezoned = meta(conn, "tz")?.as_deref() != Some(zone);
    let reversioned = meta(conn, "rollup")?.as_deref() != Some(ROLLUP_VERSION);
    if !repriced && !rezoned && !reversioned {
        return Ok(());
    }
    if repriced || reversioned {
        reread_fields(conn)?;
        model::messages(conn)?;
    }
    set_meta(conn, "tz", zone)?;
    let ids: Vec<String> = conn
        .prepare("SELECT session_id FROM sessions ORDER BY session_id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    rollup(conn, ids.iter().map(String::as_str))?;
    set_meta(conn, "rollup", ROLLUP_VERSION)?;
    set_meta(conn, "pricing_logic", cost::PRICING_LOGIC_VERSION)?;
    set_meta(conn, "prices", cost::PRICES_JSON)
}

/// Re-reads every event's accounting fields from `raw`.
fn reread_fields(conn: &Connection) -> Result<()> {
    let mut after = 0i64;
    loop {
        let rows: Vec<(i64, String)> = conn
            .prepare_cached(
                "SELECT id, raw FROM events
                 WHERE id > ?1 AND type IN ('assistant', 'user', 'queue-operation', 'attachment')
                 ORDER BY id LIMIT 1000",
            )?
            .query_map([after], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;
        let Some(&(last, _)) = rows.last() else {
            return Ok(());
        };
        for (id, raw) in rows {
            let f = fields(&serde_json::from_str(&raw).unwrap_or(Value::Null));
            conn.prepare_cached(
                "UPDATE events SET request_key = ?2, model = ?3, usage = ?4, output_tokens = ?5,
                     cost_usd = ?6, tool_uses = ?7, tool_results = ?8, prompt = ?9,
                     notification_task = ?10, notification_status = ?11, message_to = ?12,
                     api_error = ?13, sent_msg_id = ?14, peer_msg_id = ?15, peer_name = ?16
                 WHERE id = ?1",
            )?
            .execute(params![
                id,
                f.request_key,
                f.model,
                f.usage,
                f.output_tokens,
                f.cost_usd,
                f.tool_uses,
                f.tool_results,
                f.prompt,
                f.notification_task,
                f.notification_status,
                f.message_to,
                f.api_error,
                f.sent_msg_id,
                f.peer_msg_id,
                f.peer_name
            ])?;
        }
        after = last;
    }
}

/// A stored event as the time model reads it.
fn timed(
    at: &str,
    kind: Option<&str>,
    key: Option<String>,
    uses: Option<&str>,
    results: Option<&str>,
    prompt: Option<bool>,
) -> Option<Timed> {
    let at = time::parse_timestamp(at)?;
    let record = match kind {
        Some("assistant") => Record::Assistant {
            request: key,
            tool_uses: uses
                .and_then(|u| serde_json::from_str(u).ok())
                .unwrap_or_default(),
        },
        Some("user") => Record::User {
            tool_results: results
                .and_then(|r| serde_json::from_str(r).ok())
                .unwrap_or_default(),
            prompt,
        },
        _ => Record::Other,
    };
    Some(Timed { at, record })
}

/// A bucket's key: agent (`None`: the orchestrator) and local day. A
/// bucket stores one audit cell per model of its requests.
type BucketKey = (Option<String>, Option<String>);

/// The requests of one model in one bucket.
#[derive(Default)]
struct Requests {
    count: usize,
    /// The ordinal of the latest one, in time order.
    latest: usize,
    cost_usd: Option<f64>,
    unpriced: i64,
    /// Input, output, cache read, cache write.
    tokens: [i64; 4],
}

/// One (agent, day) bucket: its time and its requests per model.
#[derive(Default)]
struct Bucket {
    /// Wall, active, wait and ask ms.
    time: [i64; 4],
    models: BTreeMap<Option<String>, Requests>,
}

impl Bucket {
    fn time(&mut self, split: &Split) {
        self.time[0] += split.wall_ms;
        self.time[1] += split.active_ms();
        self.time[2] += split.user_ms;
        self.time[3] += split.ask_ms;
    }

    /// Adds request number `n` (in time order).
    fn request(&mut self, n: usize, model: Option<String>, usage: &Usage, cost_usd: Option<f64>) {
        let r = self.models.entry(model).or_default();
        r.count += 1;
        r.latest = n;
        match cost_usd {
            Some(c) => r.cost_usd = Some(r.cost_usd.unwrap_or(0.0) + c),
            None => r.unpriced += 1,
        }
        let counts = [
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_creation,
        ];
        for (sum, count) in r.tokens.iter_mut().zip(counts) {
            *sum += count as i64;
        }
    }

    /// One cell per model, the time on the model that took the most
    /// requests (the latest winning ties); a bucket with no requests is one
    /// cell with no model.
    fn cells(&self) -> Vec<(Option<&str>, [i64; 4], Option<&Requests>)> {
        let dominant = self
            .models
            .iter()
            .max_by_key(|(_, r)| (r.count, r.latest))
            .map(|(model, _)| model);
        if dominant.is_none() {
            return vec![(None, self.time, None)];
        }
        self.models
            .iter()
            .map(|(model, r)| {
                let time = if Some(model) == dominant {
                    self.time
                } else {
                    [0; 4]
                };
                (model.as_deref(), time, Some(r))
            })
            .collect()
    }
}

fn add(a: Split, b: &Split) -> Split {
    Split {
        wall_ms: a.wall_ms + b.wall_ms,
        idle_ms: a.idle_ms + b.idle_ms,
        model_ms: a.model_ms + b.model_ms,
        tools_ms: a.tools_ms + b.tools_ms,
        user_ms: a.user_ms + b.user_ms,
        ask_ms: a.ask_ms + b.ask_ms,
        other_ms: a.other_ms + b.other_ms,
    }
}

/// `time::split` of a timeline, and the same cut at each local midnight in
/// `tz`: the whole, and the buckets that sum to it.
fn split_days(
    records: &[Timed],
    also: &[i64],
    tz: &TimeZone,
) -> Option<(Split, Vec<(i64, Split)>)> {
    let buckets = time::split_at(records, also, IDLE_GAP_MS, &day_cuts(records, also, tz)?)?;
    let whole = buckets.iter().map(|(_, s)| s).fold(Split::default(), add);
    Some((whole, buckets))
}

/// Each local midnight in `tz` inside the timeline; `None` with no
/// records.
fn day_cuts(records: &[Timed], also: &[i64], tz: &TimeZone) -> Option<Vec<i64>> {
    let times = records.iter().map(|t| t.at).chain(also.iter().copied());
    let (first, last) = (times.clone().min()?, times.max()?);
    Some(time::local_midnights(tz, first, last))
}

/// A session's timelines: what its cells and runs are cut from.
struct Lines {
    /// The orchestrator's records.
    main: Vec<Timed>,
    /// Each subagent's records.
    agents: BTreeMap<String, Vec<Timed>>,
}

/// A session's timed records, each agent's in transcript order, read from
/// the `events_timeline` index alone (v12, #61). Title, cost-state and
/// ai-title records are bookkeeping, not activity.
const LINES_SQL: &str =
    "SELECT agent_id, timestamp, type, request_key, tool_uses, tool_results, prompt
     FROM events
     WHERE session_id = ?1 AND timestamp IS NOT NULL
       AND ifnull(type, '') NOT IN ('custom-title', 'ai-title', 'cost-state')
     ORDER BY agent_id, byte_offset";

impl Lines {
    fn load(conn: &Connection, session_id: &str) -> Result<Self> {
        let mut timelines: BTreeMap<Option<String>, Vec<Timed>> = BTreeMap::new();
        let mut stmt = conn.prepare_cached(LINES_SQL)?;
        let mut rows = stmt.query([session_id])?;
        while let Some(r) = rows.next()? {
            let at: String = r.get(1)?;
            let kind: Option<String> = r.get(2)?;
            let uses: Option<String> = r.get(4)?;
            let results: Option<String> = r.get(5)?;
            if let Some(t) = timed(
                &at,
                kind.as_deref(),
                r.get(3)?,
                uses.as_deref(),
                results.as_deref(),
                r.get(6)?,
            ) {
                timelines.entry(r.get(0)?).or_default().push(t);
            }
        }
        let main = timelines.remove(&None).unwrap_or_default();
        let agents = timelines
            .into_iter()
            .filter_map(|(agent, records)| Some((agent?, records)))
            .collect();
        Ok(Self { main, agents })
    }

    /// The orchestrator's `also`: its subagents' record times, so a parent
    /// waiting on a running agent is not idle.
    fn also(&self) -> Vec<i64> {
        self.agents.values().flatten().map(|t| t.at).collect()
    }
}

/// One run of a session's time (`time::runs`), placed as its audit cell
/// is: by agent (`None`: the orchestrator) and local day.
pub(super) struct Run {
    pub(super) agent: Option<String>,
    pub(super) day: Option<String>,
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) cat: Cat,
}

/// The runs of `session_id`'s timelines in `tz`, cut where [`rollup`]
/// cuts its cells, so each lies in one cell's (agent, day) and a cell's
/// time is the sum of its runs.
pub(super) fn session_runs(conn: &Connection, session_id: &str, tz: &TimeZone) -> Result<Vec<Run>> {
    let lines = Lines::load(conn, session_id)?;
    let runs = |records: &[Timed], also: &[i64]| {
        day_cuts(records, also, tz)
            .and_then(|all| time::runs(records, also, IDLE_GAP_MS, &all))
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (agent, records) in &lines.agents {
        out.extend(runs(records, &[]).into_iter().map(|(start, end, cat)| Run {
            agent: Some(agent.clone()),
            day: time::local_day(tz, start),
            start,
            end,
            cat,
        }));
    }
    let main = runs(&lines.main, &lines.also());
    out.extend(main.into_iter().map(|(start, end, cat)| Run {
        agent: None,
        day: time::local_day(tz, start),
        start,
        end,
        cat,
    }));
    Ok(out)
}

fn session(conn: &Connection, session_id: &str, tz: &TimeZone) -> Result<()> {
    let lines = Lines::load(conn, session_id)?;
    let main = &lines.main;

    let mut buckets: BTreeMap<BucketKey, Bucket> = BTreeMap::new();
    let mut agent_splits: BTreeMap<String, Split> = BTreeMap::new();
    for (agent, records) in &lines.agents {
        let Some((run, parts)) = split_days(records, &[], tz) else {
            continue;
        };
        for (at, split) in &parts {
            let key = (Some(agent.clone()), time::local_day(tz, *at));
            buckets.entry(key).or_default().time(split);
        }
        agent_splits.insert(agent.clone(), run);
    }
    let whole = split_days(main, &lines.also(), tz).map(|(whole, parts)| {
        for (at, split) in &parts {
            let key = (None, time::local_day(tz, *at));
            buckets.entry(key).or_default().time(split);
        }
        whole
    });
    // A request counts on the day of its own record.
    let mut stmt = conn.prepare_cached(
        "SELECT agent_id, timestamp, model, usage, cost_usd FROM requests
         WHERE session_id = ?1
         ORDER BY timestamp, id",
    )?;
    let mut rows = stmt.query([session_id])?;
    let mut n = 0;
    // Each agent's deepest request, as a % of its model's window.
    let mut peaks: BTreeMap<Option<String>, f64> = BTreeMap::new();
    while let Some(r) = rows.next()? {
        let agent: Option<String> = r.get(0)?;
        let at: Option<String> = r.get(1)?;
        let model: Option<String> = r.get(2)?;
        let usage: String = r.get(3)?;
        let day = at
            .as_deref()
            .and_then(time::parse_timestamp)
            .and_then(|ms| time::local_day(tz, ms));
        let usage = Usage::from_json(&serde_json::from_str(&usage).unwrap_or(Value::Null));
        if let Some(pct) = model
            .as_deref()
            .and_then(|m| cost::context_pct(usage.depth(), m))
        {
            let peak = peaks.entry(agent.clone()).or_insert(pct);
            *peak = peak.max(pct);
        }
        buckets
            .entry((agent, day))
            .or_default()
            .request(n, model, &usage, r.get(4)?);
        n += 1;
    }
    conn.prepare_cached("DELETE FROM audit_cells WHERE session_id = ?1")?
        .execute([session_id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO audit_cells (session_id, agent_id, day, wall_ms, active_ms, wait_ms,
             ask_ms, cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
             cache_write_tokens, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    for ((agent, day), bucket) in &buckets {
        for (model, [wall, active, wait, ask], r) in bucket.cells() {
            let [input, output, cache_read, cache_write] = r.map_or([0; 4], |r| r.tokens);
            insert.execute(params![
                session_id,
                agent,
                day,
                wall,
                active,
                wait,
                ask,
                r.and_then(|r| r.cost_usd),
                r.map_or(0, |r| r.unpriced),
                input,
                output,
                cache_read,
                cache_write,
                model
            ])?;
        }
    }

    let agent_ms: i64 = agent_splits.values().map(|s| s.wall_ms).sum();
    conn.prepare_cached(
        "UPDATE sessions SET
             cost_usd = (SELECT sum(cost_usd) FROM requests WHERE session_id = ?1),
             unpriced = (SELECT count(*) - count(cost_usd) FROM requests WHERE session_id = ?1),
             run_ms = ?2, active_ms = ?3, wait_ms = ?4, agent_ms = ?5
         WHERE session_id = ?1",
    )?
    .execute(params![
        session_id,
        whole.map(|s| s.wall_ms),
        whole.map(|s| s.active_ms()),
        whole.map(|s| s.user_ms),
        agent_ms
    ])?;

    let agents: Vec<(i64, Option<String>, Option<String>)> = conn
        .prepare_cached("SELECT id, agent_id, name FROM agents WHERE session_id = ?1")?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_>>()?;
    for (id, agent_id, name) in agents {
        // The orchestrator runs over its own records and waits as long as
        // its session; a subagent's run is its own.
        let (run_ms, wait_ms) = match &agent_id {
            None => {
                let own = main.iter().map(|t| t.at);
                let wall = own.clone().max().zip(own.min()).map(|(b, a)| b - a);
                (wall, whole.map(|s| s.user_ms))
            }
            Some(a) => agent_splits.get(a).map(|s| (s.wall_ms, s.user_ms)).unzip(),
        };
        let latest: Option<(String, String)> = conn
            .prepare_cached(
                "SELECT model, usage FROM events
                 WHERE session_id = ?1 AND agent_id IS ?2 AND usage IS NOT NULL
                 ORDER BY byte_offset DESC LIMIT 1",
            )?
            .query_row(params![session_id, agent_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let depth = latest.as_ref().map(|(model, usage)| {
            let depth =
                Usage::from_json(&serde_json::from_str(usage).unwrap_or(Value::Null)).depth();
            (depth as i64, cost::context_pct(depth, model))
        });
        conn.prepare_cached(
            "UPDATE agents SET
                 cost_usd = (SELECT sum(cost_usd) FROM requests
                     WHERE session_id = ?2 AND agent_id IS ?3),
                 unpriced = (SELECT count(*) - count(cost_usd) FROM requests
                     WHERE session_id = ?2 AND agent_id IS ?3),
                 run_ms = ?4, wait_ms = ?5, context_tokens = ?6, context_pct = ?7
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            session_id,
            agent_id,
            run_ms,
            wait_ms,
            depth.map(|d| d.0),
            depth.and_then(|d| d.1)
        ])?;
        let (outcome, followups) = match &agent_id {
            None => (None, None),
            Some(a) => (
                outcome(conn, session_id, a)?,
                Some(followups(conn, session_id, a, name.as_deref())?),
            ),
        };
        conn.prepare_cached(
            "UPDATE agents SET outcome = ?2, followups = ?3, context_peak_pct = ?4 WHERE id = ?1",
        )?
        .execute(params![id, outcome, followups, peaks.get(&agent_id)])?;
    }
    Ok(())
}

/// A subagent's outcome: the worst status of its task notifications in its
/// session (failed, then killed, then completed), and `blocked` when it
/// completed with a `NEEDS_CONTEXT:` line in its final report. `None`
/// when it was never notified (a background shell's notifications match no
/// agent).
fn outcome(conn: &Connection, session_id: &str, agent_id: &str) -> Result<Option<&'static str>> {
    const WORST_FIRST: [&str; 3] = ["failed", "killed", "completed"];
    let statuses: BTreeSet<String> = conn
        .prepare_cached(
            "SELECT DISTINCT notification_status FROM events
             WHERE session_id = ?1 AND notification_task = ?2",
        )?
        .query_map([session_id, agent_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    let worst = WORST_FIRST.into_iter().find(|s| statuses.contains(*s));
    if worst != Some("completed") {
        return Ok(worst);
    }
    let blocked = final_report(conn, session_id, agent_id)?.is_some_and(|r| reports_blocked(&r));
    Ok(Some(if blocked { "blocked" } else { "completed" }))
}

/// A subagent's final report: its last SubagentHandback message, else its
/// last assistant text.
fn final_report(conn: &Connection, session_id: &str, agent_id: &str) -> Result<Option<String>> {
    let handback: Option<String> = conn
        .prepare_cached(
            r#"SELECT raw FROM events
               WHERE session_id = ?1 AND agent_id = ?2 AND type = 'assistant'
                 AND tool_uses LIKE '%"SubagentHandback"%'
               ORDER BY byte_offset DESC LIMIT 1"#,
        )?
        .query_row([session_id, agent_id], |r| r.get(0))
        .optional()?;
    if let Some(raw) = handback {
        let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let message = blocks(&value)
            .iter()
            .rev()
            .filter(|b| b["type"] == "tool_use" && b["name"] == "SubagentHandback")
            .find_map(|b| b["input"]["message"].as_str());
        if let Some(m) = message {
            return Ok(Some(m.to_string()));
        }
    }
    let mut stmt = conn.prepare_cached(
        r#"SELECT raw FROM events
           WHERE session_id = ?1 AND agent_id = ?2 AND type = 'assistant'
             AND raw LIKE '%"type":"text"%'
           ORDER BY byte_offset DESC"#,
    )?;
    let mut rows = stmt.query([session_id, agent_id])?;
    while let Some(r) = rows.next()? {
        let value: Value = serde_json::from_str(&r.get::<_, String>(0)?).unwrap_or(Value::Null);
        let text: String = blocks(&value)
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        if !text.is_empty() {
            return Ok(Some(text));
        }
    }
    Ok(None)
}

/// A report with a line opening (past list, quote and emphasis marks) on
/// `NEEDS_CONTEXT:`, the harness's blocked marker. A mention mid-line is
/// not one.
fn reports_blocked(report: &str) -> bool {
    report.lines().any(|line| {
        line.trim_start_matches(|c: char| c.is_whitespace() || "-*>_`#".contains(c))
            .strip_prefix("NEEDS_CONTEXT")
            .is_some_and(|rest| rest.trim_start_matches(['*', '_', '`']).starts_with(':'))
    })
}

/// SendMessage calls anywhere in the session to the agent's id or name.
fn followups(
    conn: &Connection,
    session_id: &str,
    agent_id: &str,
    name: Option<&str>,
) -> Result<i64> {
    conn.prepare_cached(
        "SELECT count(*) FROM events AS e, json_each(e.message_to) AS t
         WHERE e.session_id = ?1 AND e.message_to IS NOT NULL AND t.value IN (?2, ?3)",
    )?
    .query_row(params![session_id, agent_id, name], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, query_plan};
    use crate::tail::test_dir;

    /// `events` rows carry their whole record inline and the timing columns
    /// sit past it, so reading them from the rows reads every record: the
    /// timeline's replay of a session reads the v12 index alone instead, in
    /// its order (#61).
    #[test]
    fn a_sessions_replay_reads_its_index_alone() {
        let dir = test_dir("replay-plan");
        let db = dir.join("strate.db");
        drop(Store::open(&db).expect("open"));
        let conn = Store::open_reader(&db).expect("reader");
        let plan = query_plan(&conn, LINES_SQL).join("\n");
        assert_eq!(
            plan,
            "SEARCH events USING COVERING INDEX events_timeline (session_id=?)"
        );
        drop(conn);
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn a_request_record_carries_its_key_model_usage_cost_and_tools() {
        let f = fields(&json!({
            "type": "assistant", "uuid": "u1", "requestId": "req_1",
            "message": {"id": "msg_1", "model": "claude-haiku-4-5-20251001",
                "content": [
                    {"type": "text", "text": "Lorem."},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}},
                    {"type": "tool_use", "id": "", "name": "Read"},
                    {"type": "tool_use", "id": "toolu_2"}
                ],
                "usage": {"input_tokens": 1000, "output_tokens": 200}}
        }));
        // haiku: 1000 * 1 + 200 * 5 = 2000 / 1e6
        assert_eq!(f.request_key.as_deref(), Some("req_1"));
        assert_eq!(f.model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(f.output_tokens, Some(200));
        assert!((f.cost_usd.expect("priced") - 0.002).abs() < 1e-12);
        assert_eq!(
            f.tool_uses.as_deref(),
            Some(r#"[["toolu_1","Bash"],["toolu_2",null]]"#)
        );
        let usage: Value = serde_json::from_str(f.usage.as_deref().expect("usage")).expect("json");
        assert_eq!(usage["input_tokens"], 1000);
    }

    #[test]
    fn the_request_key_falls_back_to_message_id_then_uuid() {
        let key = |v: Value| fields(&v).request_key;
        let msg =
            json!({"type": "assistant", "uuid": "u1", "requestId": "", "message": {"id": "msg_1"}});
        assert_eq!(key(msg).as_deref(), Some("msg_1"));
        let uuid = json!({"type": "assistant", "uuid": "u1", "message": {}});
        assert_eq!(key(uuid).as_deref(), Some("u1"));
    }

    #[test]
    fn synthetic_unpriced_and_usageless_records_are_kept_apart() {
        let synthetic = fields(&json!({
            "type": "assistant", "uuid": "u1",
            "message": {"id": "m", "model": SYNTHETIC, "usage": {"input_tokens": 0}}
        }));
        assert_eq!(synthetic.request_key.as_deref(), Some("m"), "still timed");
        assert_eq!(
            (synthetic.model, synthetic.usage),
            (None, None),
            "never a request"
        );
        let unpriced = fields(&json!({
            "type": "assistant", "uuid": "u1",
            "message": {"model": "claude-lorem-1", "usage": {"output_tokens": 9}}
        }));
        assert_eq!(unpriced.model.as_deref(), Some("claude-lorem-1"));
        assert_eq!((unpriced.output_tokens, unpriced.cost_usd), (Some(9), None));
        let usageless = fields(&json!({
            "type": "assistant", "uuid": "u1", "message": {"model": "claude-opus-5-5"}
        }));
        assert_eq!((usageless.model, usageless.usage), (None, None));
    }

    #[test]
    fn user_records_split_into_tool_results_and_prompts() {
        let result = fields(&json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Lorem."},
            {"type": "tool_result"}
        ]}}));
        assert_eq!(result.tool_results.as_deref(), Some(r#"["toolu_1",null]"#));
        assert_eq!(result.prompt, None);
        let prompt = |v: Value| fields(&v).prompt;
        assert_eq!(
            prompt(json!({"type": "user", "message": {"content": "Lorem."}})),
            Some(true)
        );
        assert_eq!(
            prompt(json!({"type": "user", "origin": {"kind": "human"},
                "message": {"content": "<task-notification>x"}})),
            Some(true),
            "origin wins over the text"
        );
        assert_eq!(
            prompt(
                json!({"type": "user", "origin": {"kind": "task-notification"},
                "message": {"content": "Lorem."}})
            ),
            Some(false)
        );
        assert_eq!(
            prompt(json!({"type": "user", "message": {"content": [
                {"type": "text", "text": "  <task-notification>"}, "x"]}})),
            Some(false),
            "no origin: the text decides"
        );
        assert_eq!(
            prompt(json!({"type": "user", "isMeta": true, "message": {"content": "Lorem."}})),
            None
        );
        assert_eq!(
            fields(&json!({"type": "system", "uuid": "u"})),
            Fields::default()
        );
    }

    const NOTE: &str = "<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>toolu_1\
        </tool-use-id>\n<status>failed</status>\n<summary>Lorem</summary>\n</task-notification>";

    fn notified(v: Value) -> (Option<String>, Option<String>) {
        let f = fields(&v);
        (f.notification_task, f.notification_status)
    }

    #[test]
    fn a_task_notification_is_read_from_each_of_its_carriers() {
        let want = (Some("a1".to_string()), Some("failed".to_string()));
        let queued = json!({"type": "queue-operation", "operation": "enqueue", "content": NOTE});
        assert_eq!(notified(queued), want);
        let attached = json!({"type": "attachment",
            "attachment": {"type": "queued_command", "prompt": NOTE}});
        assert_eq!(notified(attached), want);
        let prompt = json!({"type": "user", "origin": {"kind": "task-notification"},
            "message": {"content": NOTE}});
        assert_eq!(notified(prompt), want);
        let parts =
            json!({"type": "user", "message": {"content": [{"type": "text", "text": NOTE}]}});
        assert_eq!(notified(parts), want, "no origin: the text decides");
    }

    #[test]
    fn a_quoted_or_incomplete_notification_is_not_one() {
        let none = (None, None);
        let snapshot = json!({"type": "attachment",
            "attachment": {"type": "prompt_snapshot", "prompt": NOTE}});
        assert_eq!(notified(snapshot), none);
        let result = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": NOTE}]}});
        assert_eq!(notified(result), none);
        let typed = json!({"type": "user", "origin": {"kind": "human"},
            "message": {"content": format!("Lorem {NOTE}")}});
        assert_eq!(notified(typed), none);
        let quoted = json!({"type": "queue-operation", "content": format!("Lorem {NOTE}")});
        assert_eq!(notified(quoted), none);
        let no_status = "<task-notification>\n<task-id>a1</task-id>\n<summary>Lorem</summary>";
        assert_eq!(
            notified(json!({"type": "queue-operation", "content": no_status})),
            none
        );
        let tool_use = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": NOTE}}]}});
        assert_eq!(notified(tool_use), none);
    }

    #[test]
    fn send_messages_and_api_errors_are_read_from_assistant_records() {
        let f = fields(&json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "toolu_1", "name": "SendMessage", "input": {"to": "a1"}},
            {"type": "tool_use", "id": "toolu_2", "name": "SendMessage", "input": {"to": ""}},
            {"type": "tool_use", "id": "toolu_3", "name": "Bash", "input": {"to": "a2"}},
            {"type": "tool_use", "id": "toolu_4", "name": "SendMessage", "input": {"to": "lorem"}}
        ]}}));
        assert_eq!(f.message_to.as_deref(), Some(r#"["a1","lorem"]"#));
        assert_eq!(f.api_error, None);
        let error = |text: &str| {
            fields(
                &json!({"type": "assistant", "isApiErrorMessage": true, "error": "lorem",
                "message": {"model": SYNTHETIC, "content": [{"type": "text", "text": text}]}}),
            )
            .api_error
        };
        assert_eq!(
            error("Prompt is too long: 9 tokens > 8 maximum").as_deref(),
            Some("prompt_too_long")
        );
        assert_eq!(error("API Error: lorem").as_deref(), Some("other"));
    }

    #[test]
    fn cross_session_messages_are_read_from_user_records() {
        let sent = fields(&json!({"type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1"}]},
            "toolUseResult": {"success": true, "message": "Lorem.", "msg_id": "m1"}}));
        assert_eq!(sent.sent_msg_id.as_deref(), Some("m1"));
        assert_eq!((sent.peer_msg_id, sent.peer_name), (None, None));

        let received = fields(&json!({"type": "user", "message": {"content": "Lorem."},
            "origin": {"kind": "peer", "from": "uds:lorem", "msg_id": "m1",
                "name": "director-demo", "fromMode": "prompting"}}));
        assert_eq!(received.peer_msg_id.as_deref(), Some("m1"));
        assert_eq!(received.peer_name.as_deref(), Some("director-demo"));
        assert_eq!(received.sent_msg_id, None);

        let none = |v: Value| {
            let f = fields(&v);
            (f.sent_msg_id, f.peer_msg_id, f.peer_name)
        };
        // A failed send's result is a string; an empty id is no id.
        assert_eq!(
            none(json!({"type": "user", "toolUseResult": "Error: lorem"})),
            (None, None, None)
        );
        assert_eq!(
            none(json!({"type": "user", "toolUseResult": {"msg_id": ""}})),
            (None, None, None)
        );
        // Only a peer origin is a received message.
        assert_eq!(
            none(json!({"type": "user", "message": {"content": "Lorem."},
                "origin": {"kind": "human", "msg_id": "m1", "name": "director-demo"}})),
            (None, None, None)
        );
        // Only user records carry either.
        assert_eq!(
            none(
                json!({"type": "assistant", "message": {}, "toolUseResult": {"msg_id": "m1"},
                "origin": {"kind": "peer", "msg_id": "m1"}})
            ),
            (None, None, None)
        );
    }

    #[test]
    fn a_report_is_blocked_by_a_needs_context_line() {
        for blocked in [
            "NEEDS_CONTEXT: lorem",
            "RESULT: lorem.\n- NEEDS_CONTEXT: lorem",
            "  > **NEEDS_CONTEXT**: lorem",
            "`NEEDS_CONTEXT: lorem`",
        ] {
            assert!(reports_blocked(blocked), "{blocked:?}");
        }
        for not in [
            "RESULT: lorem, no NEEDS_CONTEXT: raised",
            "NEEDS_CONTEXT lorem",
            "NEEDS_CONTEXTUAL: lorem",
            "",
        ] {
            assert!(!reports_blocked(not), "{not:?}");
        }
    }
}
