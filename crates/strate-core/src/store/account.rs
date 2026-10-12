//! Cost and time in the store (#14). [`fields`] reads what accounting needs
//! from each record as it is inserted. Rollups are a pure function of the
//! stored events, recomputed in the batch's transaction for every session
//! the batch touches, so re-ingest is idempotent and an incremental build
//! converges to a cold one. Every function runs inside the caller's
//! transaction.

use std::collections::{BTreeMap, BTreeSet};

use jiff::tz::TimeZone;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::Result;
use crate::cost::{self, SYNTHETIC, Usage, truthy};
use crate::time::{self, IDLE_GAP_MS, Record, Split, Timed};

/// The accounting columns of one event (see `0003_cost_time.sql`).
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
}

/// Reads one record's accounting fields, as audit-lib's `add` and
/// `timeRecord` read them.
pub(super) fn fields(value: &Value) -> Fields {
    let message = &value["message"];
    let blocks = message["content"].as_array().map_or(&[][..], Vec::as_slice);
    let of_type = |kind: &'static str| blocks.iter().filter(move |b| b["type"] == kind);
    match value["type"].as_str() {
        Some("assistant") => {
            let request_key = [&value["requestId"], &message["id"], &value["uuid"]]
                .into_iter()
                .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
                .map(str::to_string);
            let tool_uses: Vec<Value> = of_type("tool_use")
                .filter_map(|b| {
                    let id = b["id"].as_str().filter(|s| !s.is_empty())?;
                    Some(json!([id, b["name"].as_str()]))
                })
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
                tool_uses: (!tool_uses.is_empty()).then(|| Value::from(tool_uses).to_string()),
                ..Fields::default()
            }
        }
        Some("user") => {
            let results: Vec<Option<&str>> = of_type("tool_result")
                .map(|b| b["tool_use_id"].as_str())
                .collect();
            let prompt = (results.is_empty() && !truthy(&value["isMeta"])).then(|| is_human(value));
            Fields {
                tool_results: (!results.is_empty()).then(|| json!(results).to_string()),
                prompt,
                ..Fields::default()
            }
        }
        _ => Fields::default(),
    }
}

/// A typed prompt, not a background agent's task notification: by
/// `origin.kind` when the record has one, else by its text.
fn is_human(value: &Value) -> bool {
    let kind = &value["origin"]["kind"];
    if truthy(kind) {
        return kind == "human";
    }
    let content = &value["message"]["content"];
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().or_else(|| p["text"].as_str()).unwrap_or(""))
            .collect(),
        _ => String::new(),
    };
    !text.trim_start().starts_with("<task-notification")
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
/// or a POSIX `TZ`), since meta `tz` stores a name.
pub(super) fn system_zone() -> String {
    TimeZone::try_system()
        .ok()
        .and_then(|tz| tz.iana_name().map(str::to_string))
        .unwrap_or_else(|| "UTC".to_string())
}

/// The zone the stored days are in. Store::open writes meta `tz` before any
/// rollup, so the UTC fallback only covers a name this build cannot load.
fn zone(conn: &Connection) -> Result<TimeZone> {
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

/// Recomputes every rollup and audit cell unless the store was last
/// accounted at the vendored prices and in `zone` (an IANA name). A new
/// price list also re-reads every event's accounting fields from `raw`. A
/// store from before v3 (no prices) or v6 (no zone) is filled here too.
pub(super) fn refresh(conn: &Connection, zone: &str) -> Result<()> {
    let repriced = meta(conn, "prices")?.as_deref() != Some(cost::PRICES_JSON);
    let rezoned = meta(conn, "tz")?.as_deref() != Some(zone);
    if !repriced && !rezoned {
        return Ok(());
    }
    if repriced {
        reread_fields(conn)?;
    }
    set_meta(conn, "tz", zone)?;
    let ids: Vec<String> = conn
        .prepare("SELECT session_id FROM sessions ORDER BY session_id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    rollup(conn, ids.iter().map(String::as_str))?;
    set_meta(conn, "prices", cost::PRICES_JSON)
}

/// Re-reads every event's accounting fields from `raw`.
fn reread_fields(conn: &Connection) -> Result<()> {
    let mut after = 0i64;
    loop {
        let rows: Vec<(i64, String)> = conn
            .prepare_cached(
                "SELECT id, raw FROM events WHERE id > ?1 AND type IN ('assistant', 'user')
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
                     cost_usd = ?6, tool_uses = ?7, tool_results = ?8, prompt = ?9
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
                f.prompt
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

/// A `/rename` window of a session transcript (one `segments` row): where it
/// starts in the transcript and, once it has a timestamped record, in time.
struct Window {
    id: i64,
    offset: i64,
    start: Option<i64>,
}

/// The session's windows in transcript order.
fn windows(conn: &Connection, session_id: &str) -> Result<Vec<Window>> {
    conn.prepare_cached(
        "SELECT id, start_offset, started_at FROM segments
         WHERE session_id = ?1 ORDER BY start_offset",
    )?
    .query_map([session_id], |r| {
        let start: Option<String> = r.get(2)?;
        Ok(Window {
            id: r.get(0)?,
            offset: r.get(1)?,
            start: start.as_deref().and_then(time::parse_timestamp),
        })
    })?
    .collect()
}

/// The window live at epoch ms `at`: the latest started by then, else the
/// first.
fn window_at(windows: &[Window], at: i64) -> Option<i64> {
    windows
        .iter()
        .rev()
        .find(|w| w.start.is_some_and(|s| s <= at))
        .or(windows.first())
        .map(|w| w.id)
}

/// The window holding transcript byte `offset`.
fn window_of(windows: &[Window], offset: i64) -> Option<i64> {
    windows
        .iter()
        .rev()
        .find(|w| w.offset <= offset)
        .map(|w| w.id)
}

/// A bucket's key: agent (`None`: the orchestrator), window, local day. A
/// bucket stores one audit cell per model of its requests.
type BucketKey = (Option<String>, Option<i64>, Option<String>);

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

/// One (agent, window, day) bucket: its time and its requests per model.
#[derive(Default)]
struct Bucket {
    /// Wall, active and wait ms.
    time: [i64; 3],
    models: BTreeMap<Option<String>, Requests>,
}

impl Bucket {
    fn time(&mut self, split: &Split) {
        self.time[0] += split.wall_ms;
        self.time[1] += split.active_ms();
        self.time[2] += split.user_ms;
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
    fn cells(&self) -> Vec<(Option<&str>, [i64; 3], Option<&Requests>)> {
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
                    [0; 3]
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
        other_ms: a.other_ms + b.other_ms,
    }
}

/// `time::split` of a timeline, and the same cut at each local midnight in
/// `tz` and at `cuts`: the whole, and the buckets that sum to it.
fn split_days(
    records: &[Timed],
    also: &[i64],
    tz: &TimeZone,
    cuts: &[i64],
) -> Option<(Split, Vec<(i64, Split)>)> {
    let times = records.iter().map(|t| t.at).chain(also.iter().copied());
    let (first, last) = (times.clone().min()?, times.max()?);
    let mut all = time::local_midnights(tz, first, last);
    all.extend_from_slice(cuts);
    let buckets = time::split_at(records, also, IDLE_GAP_MS, &all)?;
    let whole = buckets.iter().map(|(_, s)| s).fold(Split::default(), add);
    Some((whole, buckets))
}

fn session(conn: &Connection, session_id: &str, tz: &TimeZone) -> Result<()> {
    // Title, cost-state and ai-title records are bookkeeping, not activity.
    let mut timelines: BTreeMap<Option<String>, Vec<Timed>> = BTreeMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT agent_id, timestamp, type, request_key, tool_uses, tool_results, prompt
         FROM events
         WHERE session_id = ?1 AND timestamp IS NOT NULL
           AND ifnull(type, '') NOT IN ('custom-title', 'ai-title', 'cost-state')
         ORDER BY agent_id, byte_offset",
    )?;
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
    let windows = windows(conn, session_id)?;
    // A subagent counts entirely toward the window it started in.
    let started_in: BTreeMap<String, Option<i64>> = conn
        .prepare_cached(
            "SELECT agent_id, segment_id FROM agents
             WHERE session_id = ?1 AND agent_id IS NOT NULL",
        )?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    let window_of_agent = |agent: &str| started_in.get(agent).copied().flatten();

    let mut buckets: BTreeMap<BucketKey, Bucket> = BTreeMap::new();
    let mut runs: BTreeMap<String, Split> = BTreeMap::new();
    for (agent, records) in &timelines {
        let (Some(agent), Some((run, parts))) = (agent, split_days(records, &[], tz, &[])) else {
            continue;
        };
        for (at, split) in &parts {
            let key = (
                Some(agent.clone()),
                window_of_agent(agent),
                time::local_day(tz, *at),
            );
            buckets.entry(key).or_default().time(split);
        }
        runs.insert(agent.clone(), run);
    }
    // The session timeline is cut at window starts too, so a renamed
    // transcript splits per window.
    let also: Vec<i64> = timelines.values().flatten().map(|t| t.at).collect();
    let starts: Vec<i64> = windows.iter().filter_map(|w| w.start).collect();
    let whole = split_days(&main, &also, tz, &starts).map(|(whole, parts)| {
        for (at, split) in &parts {
            let key = (None, window_at(&windows, *at), time::local_day(tz, *at));
            buckets.entry(key).or_default().time(split);
        }
        whole
    });
    // A request counts on the day and in the window of its own record. An
    // orchestrator's time goes to a window by when it started, its requests
    // by transcript offset: they agree unless timestamps run out of order.
    let mut stmt = conn.prepare_cached(
        "SELECT r.agent_id, r.timestamp, r.model, r.usage, r.cost_usd, e.byte_offset
         FROM requests AS r JOIN events AS e ON e.id = r.id
         WHERE r.session_id = ?1
         ORDER BY r.timestamp, r.id",
    )?;
    let mut rows = stmt.query([session_id])?;
    let mut n = 0;
    while let Some(r) = rows.next()? {
        let agent: Option<String> = r.get(0)?;
        let at: Option<String> = r.get(1)?;
        let usage: String = r.get(3)?;
        let window = match &agent {
            None => window_of(&windows, r.get(5)?),
            Some(a) => window_of_agent(a),
        };
        let day = at
            .as_deref()
            .and_then(time::parse_timestamp)
            .and_then(|ms| time::local_day(tz, ms));
        let usage = Usage::from_json(&serde_json::from_str(&usage).unwrap_or(Value::Null));
        buckets
            .entry((agent, window, day))
            .or_default()
            .request(n, r.get(2)?, &usage, r.get(4)?);
        n += 1;
    }
    conn.prepare_cached("DELETE FROM audit_cells WHERE session_id = ?1")?
        .execute([session_id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO audit_cells (session_id, agent_id, segment_id, day, wall_ms, active_ms,
             wait_ms, cost_usd, unpriced, input_tokens, output_tokens, cache_read_tokens,
             cache_write_tokens, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    for ((agent, window, day), bucket) in &buckets {
        for (model, [wall, active, wait], r) in bucket.cells() {
            let [input, output, cache_read, cache_write] = r.map_or([0; 4], |r| r.tokens);
            insert.execute(params![
                session_id,
                agent,
                window,
                day,
                wall,
                active,
                wait,
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

    let agent_ms: i64 = runs.values().map(|s| s.wall_ms).sum();
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

    let agents: Vec<(i64, Option<String>)> = conn
        .prepare_cached("SELECT id, agent_id FROM agents WHERE session_id = ?1")?
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_>>()?;
    for (id, agent_id) in agents {
        // The orchestrator runs over its own records and waits as long as
        // its session; a subagent's run is its own.
        let (run_ms, wait_ms) = match &agent_id {
            None => {
                let own = main.iter().map(|t| t.at);
                let wall = own.clone().max().zip(own.min()).map(|(b, a)| b - a);
                (wall, whole.map(|s| s.user_ms))
            }
            Some(a) => runs.get(a).map(|s| (s.wall_ms, s.user_ms)).unzip(),
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
