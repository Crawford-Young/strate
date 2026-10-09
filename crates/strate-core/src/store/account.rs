//! Cost and time in the store (#14). [`fields`] reads what accounting needs
//! from each record as it is inserted. Rollups are a pure function of the
//! stored events, recomputed in the batch's transaction for every session
//! the batch touches, so re-ingest is idempotent and an incremental build
//! converges to a cold one. Every function runs inside the caller's
//! transaction.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use super::Result;
use crate::cost::{self, SYNTHETIC, Usage, truthy};
use crate::time::{self, IDLE_GAP_MS, Record, Timed};

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

/// Recomputes the rollups of the given sessions and their agents.
pub(super) fn rollup<'a>(conn: &Connection, ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    for id in ids {
        session(conn, id)?;
    }
    Ok(())
}

/// Re-reads every event's accounting fields from `raw` and recomputes every
/// rollup, unless the store was last accounted at the vendored prices. A
/// store from before v3 has no record of that, so it is filled here too.
pub(super) fn refresh(conn: &Connection) -> Result<()> {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'prices'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if stored.as_deref() == Some(cost::PRICES_JSON) {
        return Ok(());
    }
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
            break;
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
    let ids: Vec<String> = conn
        .prepare("SELECT session_id FROM sessions ORDER BY session_id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    rollup(conn, ids.iter().map(String::as_str))?;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('prices', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [cost::PRICES_JSON],
    )?;
    Ok(())
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

fn session(conn: &Connection, session_id: &str) -> Result<()> {
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
    let runs: BTreeMap<String, time::Split> = timelines
        .iter()
        .filter_map(|(agent, records)| {
            let split = time::split(records, &[], IDLE_GAP_MS)?;
            Some((agent.clone()?, split))
        })
        .collect();
    let also: Vec<i64> = timelines.values().flatten().map(|t| t.at).collect();
    let whole = time::split(&main, &also, IDLE_GAP_MS);
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
