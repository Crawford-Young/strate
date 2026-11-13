//! One agent's transcript, read-only, for the console (#109): the agent
//! row's own records (its session and agent id; the orchestrator's agent id
//! is NULL) from `events.raw`, in the order they were stored.
//!
//! - A `user` record is a prompt unless it is `isMeta`, a compact summary,
//!   not typed by a human (an `origin.kind` other than `human`, else a
//!   `<task-notification`), command noise (text opening `<command-` or
//!   `<local-command-`) or only carries tool results. A slash command's
//!   record reads as `/name args`. A `<teammate-message>` is the teammate's
//!   prompt, its tags stripped, as is a `<cross-session-message>`: blocks
//!   are found anywhere in the text, one entry per block in text order, its
//!   `sender` the block's `teammate_id`, or `from-name` (else `from`) for a
//!   cross-session one, none when the tag names no one; the text before,
//!   between and after blocks is an entry of its own, no sender. Every
//!   `<system-reminder>` and `<side-sessions-event>` block is stripped first;
//!   a record with nothing left is none.
//! - An `assistant` record gives one entry per text, thinking or tool_use
//!   block. A thinking block with no text (redacted: a signature only) is
//!   skipped, and so is an empty text block.
//! - A tool result (a `tool_result` block of a `user` record) completes its
//!   call by `tool_use_id`: `error` when `is_error`, else `success`, with
//!   the result's text capped at [`RESULT_MAX`] chars and the record's time.
//!   A call with no result yet is `pending`.
//! - Read by cursor (#116): a page holds the records after the last event id
//!   the console saw, and a result whose call came in an earlier page rides
//!   along as an update for the console to complete it in place.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use super::Result;

/// The longest `Tool(args)` summary, in chars.
pub const ARGS_MAX: usize = 200;
/// The longest tool result kept, in chars.
pub const RESULT_MAX: usize = 4000;

/// Where a tool call stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptToolStatus {
    /// No result yet.
    Pending,
    Success,
    /// Its result has `is_error`.
    Error,
}

/// A TodoWrite item's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptTodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One item of a TodoWrite call's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptTodo {
    pub content: String,
    pub status: TranscriptTodoStatus,
}

/// One entry of an agent's transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptEntry {
    /// `<event id>:<n>`, the record's nth entry: the same on every read.
    pub key: String,
    /// The record's ISO time.
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub body: TranscriptBody,
}

/// What an entry holds, tagged `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TranscriptBody {
    /// A prompt; `sender` names who sent a teammate or cross-session one.
    User {
        text: String,
        sender: Option<String>,
    },
    /// An assistant text block.
    Text { text: String },
    /// A thinking block with text.
    Thinking { text: String },
    ToolCall {
        /// The tool_use id.
        id: String,
        name: String,
        /// One line, `Tool(args)`, at most [`ARGS_MAX`] chars.
        args: String,
        status: TranscriptToolStatus,
        /// The result's text, at most [`RESULT_MAX`] chars.
        result: Option<String>,
        /// The result was longer than [`RESULT_MAX`] chars.
        result_truncated: bool,
        /// The result record's time.
        result_at: Option<String>,
        /// A TodoWrite call's list; `None` for any other tool.
        todos: Option<Vec<TranscriptTodo>>,
    },
}

/// A tool result for a call that is not in its page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResult {
    /// The tool_use id it answers.
    pub id: String,
    /// `success` or `error`.
    pub status: TranscriptToolStatus,
    /// At most [`RESULT_MAX`] chars.
    pub result: String,
    pub result_truncated: bool,
    /// The result record's time.
    pub result_at: Option<String>,
}

/// What an agent's transcript gained after a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptPage {
    /// The entries of the records after the cursor, every call among them
    /// completed by the results among them.
    pub entries: Vec<TranscriptEntry>,
    /// The results among them whose call is not in `entries`, in stored
    /// order: an earlier page's call, which the reader completes in place
    /// (the last one with that id), or an orphan, which matches none.
    pub updates: Vec<TranscriptResult>,
    /// The last event id read, the next read's cursor; the given cursor
    /// when nothing was.
    pub cursor: Option<i64>,
}

/// [`agent_transcript`]'s query: the agent row's own user and assistant
/// records after a cursor, by the `events_agent` index, in stored order.
const TRANSCRIPT_SQL: &str = "SELECT e.id, e.raw FROM agents AS a
     JOIN events AS e ON e.session_id = a.session_id AND e.agent_id IS a.agent_id
     WHERE a.id = ?1 AND e.type IN ('user', 'assistant') AND e.id > ?2
     ORDER BY e.id";

/// What agent row `agent`'s transcript gained after event `cursor` (all of
/// it with none); empty for an unknown agent. A page read from each page's
/// cursor in turn, its updates applied before its entries are appended,
/// adds up to the read with none: an agent's events only ever gain ids
/// above its stored ones.
pub fn agent_transcript(
    conn: &Connection,
    agent: i64,
    cursor: Option<i64>,
) -> Result<TranscriptPage> {
    let mut stmt = conn.prepare_cached(TRANSCRIPT_SQL)?;
    let rows = stmt.query_map((agent, cursor.unwrap_or(i64::MIN)), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut transcript = Transcript::default();
    let mut last = cursor;
    for row in rows {
        let (id, raw) = row?;
        transcript.push(id, &raw);
        last = Some(id);
    }
    Ok(TranscriptPage {
        entries: transcript.entries,
        updates: transcript.updates,
        cursor: last,
    })
}

/// The entries so far, and where each pending call is.
#[derive(Default)]
struct Transcript {
    entries: Vec<TranscriptEntry>,
    updates: Vec<TranscriptResult>,
    calls: HashMap<String, usize>,
    /// The record being read: its event id, and how many entries it gave.
    event: i64,
    given: usize,
}

impl Transcript {
    /// Adds the entries of one raw record, event `event`; a record that is
    /// not JSON adds nothing.
    fn push(&mut self, event: i64, raw: &str) {
        self.event = event;
        self.given = 0;
        let Ok(record) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        let timestamp = record["timestamp"].as_str().map(str::to_string);
        let content = &record["message"]["content"];
        match record["type"].as_str() {
            Some("assistant") => self.assistant(content, timestamp),
            Some("user")
                if !flagged(&record, "isMeta") && !flagged(&record, "isCompactSummary") =>
            {
                self.user(content, timestamp, is_human(&record));
            }
            _ => {}
        }
    }

    fn add(&mut self, timestamp: &Option<String>, body: TranscriptBody) {
        self.entries.push(TranscriptEntry {
            key: format!("{}:{}", self.event, self.given),
            timestamp: timestamp.clone(),
            body,
        });
        self.given += 1;
    }

    fn assistant(&mut self, content: &Value, timestamp: Option<String>) {
        if let Some(text) = content.as_str().filter(|t| !t.trim().is_empty()) {
            self.add(&timestamp, TranscriptBody::Text { text: text.into() });
        }
        for block in content.as_array().into_iter().flatten() {
            let text = |key: &str| {
                block[key]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
            };
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = text("text") {
                        self.add(&timestamp, TranscriptBody::Text { text });
                    }
                }
                Some("thinking") => {
                    if let Some(text) = text("thinking") {
                        self.add(&timestamp, TranscriptBody::Thinking { text });
                    }
                }
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or_default().to_string();
                    let name = block["name"].as_str().unwrap_or_default().to_string();
                    let input = &block["input"];
                    // A result can name no call that has no id.
                    if !id.is_empty() {
                        self.calls.insert(id.clone(), self.entries.len());
                    }
                    self.add(
                        &timestamp,
                        TranscriptBody::ToolCall {
                            args: summary(&name, input),
                            todos: (name == "TodoWrite").then(|| todos(input)),
                            id,
                            name,
                            status: TranscriptToolStatus::Pending,
                            result: None,
                            result_truncated: false,
                            result_at: None,
                        },
                    );
                }
                _ => {}
            }
        }
    }

    /// The record's tool results, and its prompt if `human` typed one, with
    /// its reminder and side-session blocks stripped.
    fn user(&mut self, content: &Value, timestamp: Option<String>, human: bool) {
        let mut texts = Vec::new();
        match content {
            Value::String(text) => texts.push(text.as_str()),
            _ => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("tool_result") => self.result(block, &timestamp),
                        Some("text") => texts.extend(block["text"].as_str()),
                        _ => {}
                    }
                }
            }
        }
        if !human {
            return;
        }
        let text = without_blocks(
            &without_blocks(&texts.join("\n"), "system-reminder"),
            "side-sessions-event",
        );
        let trimmed = text.trim();
        if let Some(command) = slash_command(trimmed) {
            self.add(
                &timestamp,
                TranscriptBody::User {
                    text: command,
                    sender: None,
                },
            );
        } else if let Some(blocks) = tag_blocks(trimmed) {
            // One entry per block, each with its own sender, and one per
            // run of text around them.
            for (sender, text) in blocks {
                self.add(&timestamp, TranscriptBody::User { text, sender });
            }
        } else if !trimmed.is_empty()
            && !trimmed.starts_with("<side-sessions-event")
            && !trimmed.starts_with("<command-")
            && !trimmed.starts_with("<local-command-")
        {
            self.add(
                &timestamp,
                TranscriptBody::User {
                    text: trimmed.to_string(),
                    sender: None,
                },
            );
        }
    }

    /// Completes the call `block` (a tool_result) answers if it is in the
    /// page, else keeps it as an update; a result with no id answers none.
    fn result(&mut self, block: &Value, timestamp: &Option<String>) {
        let Some(id) = block["tool_use_id"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let (text, truncated) = capped(result_text(&block["content"]), RESULT_MAX);
        let done = TranscriptResult {
            id: id.to_string(),
            status: if flagged(block, "is_error") {
                TranscriptToolStatus::Error
            } else {
                TranscriptToolStatus::Success
            },
            result: text,
            result_truncated: truncated,
            result_at: timestamp.clone(),
        };
        match self.calls.get(id) {
            Some(&at) => done.complete(&mut self.entries[at].body),
            None => self.updates.push(done),
        }
    }
}

impl TranscriptResult {
    /// Completes `body` with this result, when it is a call.
    fn complete(self, body: &mut TranscriptBody) {
        if let TranscriptBody::ToolCall {
            status,
            result,
            result_truncated,
            result_at,
            ..
        } = body
        {
            *status = self.status;
            *result = Some(self.result);
            *result_truncated = self.result_truncated;
            *result_at = self.result_at;
        }
    }
}

fn flagged(value: &Value, key: &str) -> bool {
    value[key].as_bool() == Some(true)
}

/// A typed prompt, not a background agent's task notification: by
/// `origin.kind` when the record has one, else by its text. The account's
/// rule (`is_human` in `store/account.rs`), mirrored as that one is private.
fn is_human(record: &Value) -> bool {
    let kind = &record["origin"]["kind"];
    if crate::cost::truthy(kind) {
        return kind == "human";
    }
    let content = &record["message"]["content"];
    let text = match content {
        Value::String(text) => text.clone(),
        _ => content
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| p.as_str().or_else(|| p["text"].as_str()).unwrap_or(""))
            .collect(),
    };
    !text.trim_start().starts_with("<task-notification")
}

/// The slash command `text` (a prompt's, trimmed at the start) invokes,
/// as `/name args`: its `<command-name>`, then its `<command-args>` with
/// the ends trimmed, if any.
fn slash_command(text: &str) -> Option<String> {
    let tag = |name: &str| {
        let (_, rest) = text.split_once(&format!("<{name}>"))?;
        let (inner, _) = rest.split_once(&format!("</{name}>"))?;
        Some(inner.trim())
    };
    if !text.starts_with("<command-") {
        return None;
    }
    let name = tag("command-name").filter(|n| !n.is_empty())?;
    let slash = if name.starts_with('/') { "" } else { "/" };
    Some(match tag("command-args").unwrap_or_default() {
        "" => format!("{slash}{name}"),
        args => format!("{slash}{name} {args}"),
    })
}

/// The index of the `>` ending an open tag's attributes in `s`, skipping any
/// inside a quoted (`"` or `'`) attribute value.
fn open_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if q == c => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// The first of `names` that the open tag's `attrs` gives a non-empty
/// value, read as `name=` then a single- or double-quoted value, after any
/// whitespace; a value's own text is never read as an attribute.
fn attribute(attrs: &str, names: &[&str]) -> Option<String> {
    let mut found: Vec<(&str, &str)> = Vec::new();
    let mut rest = attrs;
    loop {
        rest = rest.trim_start();
        let name_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        let (name, after) = rest.split_at(name_end);
        let Some(after) = after.trim_start().strip_prefix('=') else {
            if name.is_empty() {
                break;
            }
            rest = after;
            continue;
        };
        let after = after.trim_start();
        let Some(quote) = after.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            break;
        };
        let Some((value, next)) = after[1..].split_once(quote) else {
            break;
        };
        found.push((name, value));
        rest = next;
    }
    names.iter().find_map(|name| {
        found
            .iter()
            .find(|(n, v)| n == name && !v.is_empty())
            .map(|(_, v)| v.to_string())
    })
}

/// The first closed `<tag ...>…</tag>` block in `text`: where it starts and
/// ends, its open tag's attributes and its inner text. The tag name ends at
/// whitespace or `>`, so `<tag-s` is another tag.
fn next_block<'a>(text: &'a str, tag: &str) -> Option<(usize, usize, &'a str, &'a str)> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut from = 0;
    while let Some(at) = text[from..].find(&open).map(|i| i + from) {
        from = at + open.len();
        let after = &text[from..];
        if !after.starts_with(|c: char| c == '>' || c.is_whitespace()) {
            continue;
        }
        let Some(end) = open_end(after) else {
            continue;
        };
        let body = from + end + 1;
        if let Some(c) = text[body..].find(&close) {
            return Some((
                at,
                body + c + close.len(),
                &after[..end],
                &text[body..body + c],
            ));
        }
    }
    None
}

/// Each prompt entry `text` holds when it has a teammate-message or
/// cross-session-message block anywhere: a block as its sender (the first of
/// its tag's sender attributes its open tag carries, if any) and its inner
/// text, trimmed, and each run of text around the blocks as a sender-less
/// one, in text order; empty ones are skipped. `None` if there is no block.
fn tag_blocks(text: &str) -> Option<Vec<(Option<String>, String)>> {
    const TAGS: [(&str, &[&str]); 2] = [
        ("teammate-message", &["teammate_id"]),
        ("cross-session-message", &["from-name", "from"]),
    ];
    let mut out = Vec::new();
    let mut rest = text;
    let mut any = false;
    let mut keep = |sender: Option<String>, run: &str| {
        let run = run.trim();
        if !run.is_empty() {
            out.push((sender, run.to_string()));
        }
    };
    while let Some((senders, (start, end, attrs, body))) = TAGS
        .iter()
        .filter_map(|(tag, senders)| next_block(rest, tag).map(|b| (*senders, b)))
        .min_by_key(|(_, b)| b.0)
    {
        any = true;
        keep(None, &rest[..start]);
        keep(attribute(attrs, senders), body);
        rest = &rest[end..];
    }
    keep(None, rest);
    any.then_some(out)
}

/// `text` without its closed `<tag ...>…</tag>` blocks, wherever they sit.
fn without_blocks(text: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = text.to_string();
    let mut from = 0;
    while let Some(at) = out[from..].find(&open).map(|i| i + from) {
        let after = &out[at + open.len()..];
        let named = after.starts_with(|c: char| c == '>' || c.is_whitespace());
        let end = named.then(|| open_end(after)).flatten().and_then(|e| {
            let body = at + open.len() + e + 1;
            out[body..].find(&close).map(|c| body + c + close.len())
        });
        match end {
            Some(end) => out.replace_range(at..end, ""),
            None => from = at + open.len(),
        }
    }
    out
}

/// A tool result's text: a string as it is; blocks' texts on lines of
/// their own, any other block as `[<type>]`.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match (b["type"].as_str(), b["text"].as_str()) {
                (Some("text"), Some(text)) => text.to_string(),
                (kind, _) => format!("[{}]", kind.unwrap_or("block")),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `text` cut to its first `max` chars, and whether it was cut.
fn capped(text: String, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (text[..end].to_string(), true),
        None => (text, false),
    }
}

/// The input keys whose value is a call's argument, in preference order.
const ARG_KEYS: [&str; 10] = [
    "command",
    "file_path",
    "notebook_path",
    "path",
    "pattern",
    "url",
    "query",
    "description",
    "skill",
    "prompt",
];

/// One line, `Tool(args)`, of at most [`ARGS_MAX`] chars: the first of
/// [`ARG_KEYS`] the input has as a string, a TodoWrite's item count, else
/// the input's compact JSON; whitespace runs collapse to one space, and a
/// long argument ends in `…`.
fn summary(name: &str, input: &Value) -> String {
    let arg = if name == "TodoWrite" {
        format!("{} todos", input["todos"].as_array().map_or(0, Vec::len))
    } else if let Some(arg) = ARG_KEYS.iter().find_map(|k| input[k].as_str()) {
        arg.to_string()
    } else if input.as_object().is_some_and(|o| !o.is_empty()) {
        input.to_string()
    } else {
        String::new()
    };
    let arg = arg.split_whitespace().collect::<Vec<_>>().join(" ");
    // The name leaves room for the parentheses and one char of argument.
    let name = ellipsized(name.to_string(), ARGS_MAX - 3);
    let room = ARGS_MAX - name.chars().count() - 2;
    format!("{name}({})", ellipsized(arg, room))
}

/// `text` if it has at most `max` (> 0) chars, else its first `max - 1`
/// and `…`.
fn ellipsized(text: String, max: usize) -> String {
    match capped(text, max) {
        (text, false) => text,
        (mut text, true) => {
            text.pop();
            text.push('…');
            text
        }
    }
}

/// A TodoWrite input's items; an unknown status reads pending.
fn todos(input: &Value) -> Vec<TranscriptTodo> {
    input["todos"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| TranscriptTodo {
            content: t["content"].as_str().unwrap_or_default().to_string(),
            status: match t["status"].as_str() {
                Some("in_progress") => TranscriptTodoStatus::InProgress,
                Some("completed") => TranscriptTodoStatus::Completed,
                _ => TranscriptTodoStatus::Pending,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The entries of `records`, each serialized as a JSON line, keys
    /// blanked: the parsing tests compare what an entry says.
    fn entries(records: &[Value]) -> Vec<TranscriptEntry> {
        let mut t = Transcript::default();
        for (i, r) in (1..).zip(records) {
            t.push(i, &r.to_string());
        }
        t.entries
            .into_iter()
            .map(|e| TranscriptEntry {
                key: String::new(),
                ..e
            })
            .collect()
    }

    fn at(n: u32) -> String {
        format!("2026-10-30T10:00:{n:02}.000Z")
    }

    fn user(n: u32, content: Value) -> Value {
        json!({"type": "user", "timestamp": at(n), "message": {"role": "user", "content": content}})
    }

    fn assistant(n: u32, blocks: Value) -> Value {
        json!({"type": "assistant", "timestamp": at(n),
            "message": {"role": "assistant", "content": blocks}})
    }

    fn tool_use(n: u32, id: &str, name: &str, input: Value) -> Value {
        assistant(
            n,
            json!([{"type": "tool_use", "id": id, "name": name, "input": input}]),
        )
    }

    fn result(n: u32, id: &str, content: Value, is_error: bool) -> Value {
        user(
            n,
            json!([{"type": "tool_result", "tool_use_id": id, "content": content,
                "is_error": is_error}]),
        )
    }

    fn entry(n: u32, body: TranscriptBody) -> TranscriptEntry {
        TranscriptEntry {
            key: String::new(),
            timestamp: Some(at(n)),
            body,
        }
    }

    fn said(n: u32, text: &str) -> TranscriptEntry {
        entry(
            n,
            TranscriptBody::User {
                text: text.into(),
                sender: None,
            },
        )
    }

    /// A prompt entry whose sender is `sender`.
    fn sent(n: u32, text: &str, sender: &str) -> TranscriptEntry {
        entry(
            n,
            TranscriptBody::User {
                text: text.into(),
                sender: Some(sender.into()),
            },
        )
    }

    #[test]
    fn prompts_are_user_entries_and_meta_command_and_summary_records_are_not() {
        let meta = json!({"type": "user", "isMeta": true, "timestamp": at(2),
            "message": {"role": "user", "content": "Lorem caveat."}});
        let summary = json!({"type": "user", "isCompactSummary": true, "timestamp": at(3),
            "message": {"role": "user", "content": "Lorem summary."}});
        let got = entries(&[
            user(1, json!("Lorem ipsum.")),
            meta,
            summary,
            user(
                4,
                json!("<local-command-caveat>Lorem caveat.</local-command-caveat>"),
            ),
            user(
                5,
                json!("  <local-command-stdout>Lorem</local-command-stdout>"),
            ),
            user(
                6,
                json!([{"type": "text", "text": "Dolor"}, {"type": "image", "source": {}},
                {"type": "text", "text": "sit."}]),
            ),
            user(7, json!([{"type": "image", "source": {}}])),
            user(8, json!("   ")),
            json!({"type": "system", "timestamp": at(9), "content": "Lorem."}),
            json!("not a record"),
        ]);
        assert_eq!(got, [said(1, "Lorem ipsum."), said(6, "Dolor\nsit.")]);
        assert!(entries(&[]).is_empty());
        let mut t = Transcript::default();
        t.push(1, "{not json");
        assert!(t.entries.is_empty());
    }

    /// A `user` record with `origin`.
    fn from(n: u32, kind: &str, content: Value) -> Value {
        let mut record = user(n, content);
        record["origin"] = json!({"kind": kind});
        record
    }

    #[test]
    fn a_record_nobody_typed_is_no_prompt() {
        let notice = "<task-notification>\n<task-id>a1</task-id>\n<status>completed</status>\n\
                      </task-notification>";
        let got = entries(&[
            user(1, json!(notice)),
            user(2, json!([{"type": "text", "text": notice}])),
            from(3, "task-notification", json!("Lorem.")),
            from(4, "peer", json!("Ipsum.")),
            from(5, "human", json!("Dolor.")),
            // By origin when there is one, as the account reads it.
            from(6, "human", json!(notice)),
        ]);
        assert_eq!(got, [said(5, "Dolor."), said(6, notice)]);
    }

    #[test]
    fn a_slash_command_is_a_prompt_of_its_name_and_args() {
        let command = |name: &str, args: &str| {
            json!(format!(
                "<command-name>{name}</command-name>\n<command-message>{name}</command-message>\n\
                 <command-args>{args}</command-args>"
            ))
        };
        let got = entries(&[
            user(1, command("/clear", "")),
            user(2, command("/review", "  lorem\n ipsum ")),
            user(3, command("/x", "   ")),
            from(4, "human", command("/y", "dolor")),
            user(
                5,
                json!("<local-command-stdout>Lorem</local-command-stdout>"),
            ),
        ]);
        assert_eq!(
            got,
            [
                said(1, "/clear"),
                said(2, "/review lorem\n ipsum"),
                said(3, "/x"),
                said(4, "/y dolor"),
            ]
        );
    }

    #[test]
    fn a_teammate_message_is_the_teammates_prompt_with_its_wrapper_stripped() {
        let msg = |attrs: &str, inner: &str| {
            format!("<teammate-message {attrs}>\n{inner}\n</teammate-message>")
        };
        let two = format!(
            "{}\n\n{}",
            msg(
                "teammate_id=\"team-lead\" summary=\"Lorem\"",
                "  Lorem ipsum.  "
            ),
            msg("teammate_id=\"team-lead\"", "Dolor sit.")
        );
        let got = entries(&[
            user(1, json!(msg("teammate_id=\"team-lead\"", "Lorem ipsum."))),
            user(2, json!(two)),
            user(3, json!(msg("teammate_id=\"team-lead\"", "  "))),
        ]);
        assert_eq!(
            got,
            [
                sent(1, "Lorem ipsum.", "team-lead"),
                sent(2, "Lorem ipsum.", "team-lead"),
                sent(2, "Dolor sit.", "team-lead"),
            ]
        );
    }

    #[test]
    fn a_sender_is_the_teammate_id_else_the_cross_session_from_name_else_from() {
        let cross = |attrs: &str| {
            json!(format!(
                "<cross-session-message {attrs}>\nLorem.\n</cross-session-message>"
            ))
        };
        let got = entries(&[
            user(1, cross("from=\"a\" from-name=\"\"")),
            user(2, cross("from-name=\"b\"")),
            user(3, cross("summary=\"x\"")),
            user(
                4,
                json!(
                    "<teammate-message teammate_id=\"\" from=\"z\">\nLorem.\n</teammate-message>"
                ),
            ),
        ]);
        let sender: Vec<_> = got
            .iter()
            .map(|e| match &e.body {
                TranscriptBody::User { sender, .. } => sender.as_deref(),
                _ => unreachable!("prompts only"),
            })
            .collect();
        assert_eq!(
            sender,
            [Some("a"), Some("b"), None, None],
            "an empty or missing attribute names no one; a teammate has no `from`"
        );
    }

    fn tm(attrs: &str, inner: &str) -> String {
        format!("<teammate-message{attrs}>\n{inner}\n</teammate-message>")
    }

    #[test]
    fn blocks_are_found_anywhere_and_the_text_around_them_is_kept_in_order() {
        let pre = format!(
            "Lorem ipsum:\n\n{}\n\n{}",
            tm(" teammate_id=\"a\"", "One."),
            tm(" teammate_id=\"b\"", "Two.")
        );
        let post = format!("{}\n\nDolor sit.\n", tm(" teammate_id=\"a\"", "One."));
        let mid = format!(
            "{} between {}",
            tm(" teammate_id=\"a\"", "One."),
            tm(" teammate_id=\"b\"", "Two.")
        );
        let mixed = format!(
            "{}\n\n<cross-session-message from=\"uds:x\" from-name=\"xx\">\nTwo.\n</cross-session-message>",
            tm(" teammate_id=\"a\"", "One.")
        );
        let got = entries(&[
            user(1, json!(pre)),
            user(2, json!(post)),
            user(3, json!(mid)),
            user(4, json!(mixed)),
        ]);
        assert_eq!(
            got,
            [
                said(1, "Lorem ipsum:"),
                sent(1, "One.", "a"),
                sent(1, "Two.", "b"),
                sent(2, "One.", "a"),
                said(2, "Dolor sit."),
                sent(3, "One.", "a"),
                said(3, "between"),
                sent(3, "Two.", "b"),
                sent(4, "One.", "a"),
                sent(4, "Two.", "xx"),
            ]
        );
    }

    #[test]
    fn an_attribute_may_follow_any_whitespace_and_be_single_quoted() {
        let got = entries(&[
            user(1, json!(tm("\n  teammate_id='a'", "One."))),
            user(2, json!(tm(" summary=\"it's\" teammate_id=\"b\"", "Two."))),
            user(
                3,
                json!(tm(
                    " summary=\"say teammate_id=\" teammate_id=\"c\"",
                    "Three."
                )),
            ),
            user(4, json!(tm(" teammate_id=''", "Four."))),
        ]);
        let senders: Vec<_> = got
            .iter()
            .map(|e| match &e.body {
                TranscriptBody::User { sender, .. } => sender.clone(),
                _ => unreachable!("prompts only"),
            })
            .collect();
        assert_eq!(
            senders,
            [Some("a".into()), Some("b".into()), Some("c".into()), None]
        );
    }

    #[test]
    fn a_tag_name_needs_a_boundary_and_a_quoted_open_tag_splits_nothing() {
        let quoted = tm(
            " teammate_id=\"a\"",
            "Quoting <teammate-message teammate_id=\"z\"> here",
        );
        let longer = "<teammate-messages teammate_id=\"a\">\nLorem.\n</teammate-messages>";
        let got = entries(&[user(1, json!(quoted)), user(2, json!(longer))]);
        assert_eq!(
            got,
            [
                sent(1, "Quoting <teammate-message teammate_id=\"z\"> here", "a"),
                said(2, longer),
            ]
        );
    }

    /// The synthetic sender session (`generate.sh`): `(key, text, sender)` of
    /// each prompt entry.
    #[test]
    fn the_sender_fixture_gives_one_entry_per_tagged_block_each_with_its_sender() {
        let fixture = include_str!(
            "../../tests/fixtures/sender-home/projects/-work-sender/5e55c1c1-0000-4000-8000-0000000000f1.jsonl"
        );
        let mut t = Transcript::default();
        for (i, line) in (1..).zip(fixture.lines()) {
            t.push(i, line);
        }
        let got: Vec<_> = t
            .entries
            .iter()
            .map(|e| match &e.body {
                TranscriptBody::User { text, sender } => {
                    (e.key.as_str(), text.as_str(), sender.as_deref())
                }
                _ => unreachable!("prompts only"),
            })
            .collect();
        assert_eq!(
            got,
            [
                ("1:0", "Lorem ipsum dolor.", None),
                ("2:0", "Lorem ipsum.", Some("lorem-mate")),
                ("3:0", "Dolor sit.", Some("ipsum-lead")),
                ("4:0", "Amet elit.", Some("uds:amet")),
                ("5:0", "First.", Some("lorem-mate")),
                ("5:1", "Second.", Some("ipsum-mate")),
                ("6:0", "Third.", Some("one")),
                ("6:1", "Fourth.", Some("uds:b")),
                ("7:0", "Nameless.", None),
                ("8:0", "Lorem preamble:", None),
                ("8:1", "Pre one.", Some("lorem-mate")),
                ("8:2", "Pre two.", Some("ipsum-mate")),
                ("9:0", "Body.", Some("lorem-mate")),
                ("9:1", "Trailing text.", None),
                ("10:0", "Mate.", Some("lorem-mate")),
                ("10:1", "Cross.", Some("sit-lead")),
            ]
        );
    }

    #[test]
    fn a_lone_system_reminder_is_no_prompt_typed_or_not() {
        let reminder = "<system-reminder>\nLorem.\n</system-reminder>";
        let got = entries(&[
            user(1, json!(reminder)),
            from(2, "human", json!(reminder)),
            from(
                3,
                "human",
                json!("<artifact-view-context>Lorem</artifact-view-context>"),
            ),
        ]);
        assert_eq!(
            got,
            [said(
                3,
                "<artifact-view-context>Lorem</artifact-view-context>"
            )]
        );
    }

    #[test]
    fn reminder_blocks_are_stripped_and_what_follows_is_read_on_its_own() {
        let reminder = "<system-reminder>\nLorem.\n</system-reminder>";
        let event = "<side-sessions-event kind=\"x\">Ipsum.</side-sessions-event>";
        let cross = |attrs: &str, inner: &str| {
            format!("<cross-session-message {attrs}>\n{inner}\n</cross-session-message>")
        };
        let two = format!(
            "{reminder}\n{}\n\n{}",
            cross("from=\"a\" from-name=\"b\"", "  Dolor sit.  "),
            cross("from=\"a\"", "Amet.")
        );
        let got = entries(&[
            user(1, json!(format!("{reminder}\n{reminder}\nConsectetur."))),
            user(2, json!(format!("{reminder}\n{event}"))),
            user(3, json!(two)),
            user(
                4,
                json!(format!("{reminder}\n{}", cross("from=\"a\"", "  "))),
            ),
            user(
                5,
                json!(format!(
                    "{reminder}\n{}",
                    cross("from=\"a\" summary=\"x > y\"", "Elit.")
                )),
            ),
            from(6, "human", json!(format!("{reminder}\nAdipiscing."))),
            from(7, "human", json!(reminder)),
        ]);
        assert_eq!(
            got,
            [
                said(1, "Consectetur."),
                sent(3, "Dolor sit.", "b"),
                sent(3, "Amet.", "a"),
                sent(5, "Elit.", "a"),
                said(6, "Adipiscing."),
            ]
        );
    }

    #[test]
    fn a_slash_command_reads_the_same_with_its_message_tag_first() {
        let command = "<command-message>lorem</command-message>\n<command-name>/lorem</command-name>\n\
                       <command-args>ipsum</command-args>";
        let got = entries(&[user(1, json!(command)), from(2, "human", json!(command))]);
        assert_eq!(got, [said(1, "/lorem ipsum"), said(2, "/lorem ipsum")]);
    }

    #[test]
    fn assistant_blocks_are_entries_and_redacted_thinking_is_skipped() {
        let got = entries(&[
            assistant(
                1,
                json!([{"type": "thinking", "thinking": "", "signature": "c2lnbmF0dXJl"}]),
            ),
            assistant(2, json!([{"type": "thinking", "thinking": "Lorem."}])),
            assistant(
                3,
                json!([{"type": "text", "text": "Ipsum."}, {"type": "text", "text": " \n"},
                {"type": "redacted_thinking", "data": "x"}]),
            ),
            assistant(4, json!("Dolor.")),
        ]);
        assert_eq!(
            got,
            [
                entry(
                    2,
                    TranscriptBody::Thinking {
                        text: "Lorem.".into()
                    }
                ),
                entry(
                    3,
                    TranscriptBody::Text {
                        text: "Ipsum.".into()
                    }
                ),
                entry(
                    4,
                    TranscriptBody::Text {
                        text: "Dolor.".into()
                    }
                ),
            ]
        );
    }

    /// A call's entry: `(id, args, status, result, truncated, result_at)`.
    fn call(
        e: &TranscriptEntry,
    ) -> (
        &str,
        &str,
        TranscriptToolStatus,
        Option<&str>,
        bool,
        Option<&str>,
    ) {
        let TranscriptBody::ToolCall {
            id,
            args,
            status,
            result,
            result_truncated,
            result_at,
            ..
        } = &e.body
        else {
            panic!("not a call: {e:?}");
        };
        (
            id,
            args,
            *status,
            result.as_deref(),
            *result_truncated,
            result_at.as_deref(),
        )
    }

    #[test]
    fn a_tool_result_completes_its_call_by_id() {
        use TranscriptToolStatus::*;
        let long = "x".repeat(RESULT_MAX + 1);
        let got = entries(&[
            tool_use(1, "t1", "Bash", json!({"command": "ls -la\n  /work/x"})),
            tool_use(2, "t2", "Read", json!({"file_path": "/work/x/a.rs"})),
            tool_use(3, "t3", "Grep", json!({"pattern": "lorem"})),
            tool_use(4, "t4", "WebFetch", json!({"url": "https://example.com"})),
            result(5, "t1", json!("Lorem."), false),
            result(
                6,
                "t2",
                json!([{"type": "text", "text": "Ipsum"}, {"type": "image", "source": {}}]),
                true,
            ),
            result(7, "t3", json!(long), false),
            result(8, "t9", json!("An orphan."), false),
        ]);
        let calls: Vec<_> = got.iter().map(call).collect();
        let kept = "x".repeat(RESULT_MAX);
        assert_eq!(
            calls,
            [
                (
                    "t1",
                    "Bash(ls -la /work/x)",
                    Success,
                    Some("Lorem."),
                    false,
                    Some(at(5).as_str())
                ),
                (
                    "t2",
                    "Read(/work/x/a.rs)",
                    Error,
                    Some("Ipsum\n[image]"),
                    false,
                    Some(at(6).as_str())
                ),
                (
                    "t3",
                    "Grep(lorem)",
                    Success,
                    Some(kept.as_str()),
                    true,
                    Some(at(7).as_str())
                ),
                (
                    "t4",
                    "WebFetch(https://example.com)",
                    Pending,
                    None,
                    false,
                    None
                ),
            ],
            "a tool_result carrier is no prompt, and an orphan result is dropped"
        );
        assert_eq!(got[0].timestamp, Some(at(1)), "the call's own time");
    }

    #[test]
    fn a_call_with_no_id_is_never_completed() {
        use TranscriptToolStatus::*;
        let got = entries(&[
            tool_use(1, "", "Bash", json!({"command": "ls"})),
            assistant(
                2,
                json!([{"type": "tool_use", "name": "Read", "input": {"file_path": "/work/x"}}]),
            ),
            result(3, "", json!("Lorem."), false),
            user(
                4,
                json!([{"type": "tool_result", "content": "Ipsum.", "is_error": false}]),
            ),
        ]);
        let calls: Vec<_> = got.iter().map(call).collect();
        assert_eq!(
            calls,
            [
                ("", "Bash(ls)", Pending, None, false, None),
                ("", "Read(/work/x)", Pending, None, false, None),
            ]
        );
    }

    #[test]
    fn a_calls_args_are_one_line_of_at_most_args_max_chars() {
        let args = |name: &str, input: Value| {
            let got = entries(&[tool_use(1, "t", name, input)]);
            call(&got[0]).1.to_string()
        };
        assert_eq!(
            args("Agent", json!({"description": "Lorem", "prompt": "Ipsum"})),
            "Agent(Lorem)"
        );
        assert_eq!(
            args("Mystery", json!({"a": 1, "b": "c"})),
            r#"Mystery({"a":1,"b":"c"})"#
        );
        assert_eq!(args("Mystery", json!({})), "Mystery()");
        assert_eq!(args("Mystery", Value::Null), "Mystery()");
        let long = args("Bash", json!({"command": "é".repeat(ARGS_MAX)}));
        assert_eq!(long.chars().count(), ARGS_MAX);
        assert!(
            long.starts_with("Bash(é") && long.ends_with("é…)"),
            "{long}"
        );
    }

    #[test]
    fn a_long_tool_name_keeps_the_call_shape() {
        let args = |name: &str, input: Value| {
            let got = entries(&[tool_use(1, "t", name, input)]);
            call(&got[0]).1.to_string()
        };
        for len in [ARGS_MAX - 3, ARGS_MAX - 2, ARGS_MAX, ARGS_MAX + 50] {
            let name = "N".repeat(len);
            for input in [json!({"command": "ls"}), json!({})] {
                let got = args(&name, input.clone());
                assert!(
                    got.chars().count() <= ARGS_MAX && got.starts_with('N') && got.ends_with(')'),
                    "{len} {input}: {got}"
                );
                assert!(got.contains('('), "{len} {input}: {got}");
            }
        }
        let long = args(&"N".repeat(ARGS_MAX), json!({"command": "ls"}));
        assert_eq!(long.chars().count(), ARGS_MAX);
        assert!(long.ends_with("N…(…)"), "{long}");
    }

    #[test]
    fn a_todowrite_call_carries_its_list() {
        let got = entries(&[
            tool_use(
                1,
                "t1",
                "TodoWrite",
                json!({"todos": [
                    {"content": "Lorem", "status": "completed", "activeForm": "Loreming"},
                    {"content": "Ipsum", "status": "in_progress", "activeForm": "Ipsuming"},
                    {"content": "Dolor", "status": "pending", "activeForm": "Doloring"},
                    {"content": "Sit", "status": "lorem"},
                    {"status": "pending"},
                ]}),
            ),
            tool_use(2, "t2", "Bash", json!({"command": "ls"})),
        ]);
        let todo = |content: &str, status| TranscriptTodo {
            content: content.into(),
            status,
        };
        use TranscriptTodoStatus::*;
        let TranscriptBody::ToolCall { args, todos, .. } = &got[0].body else {
            panic!("a call");
        };
        assert_eq!(args, "TodoWrite(5 todos)");
        assert_eq!(
            todos.as_deref(),
            Some(
                &[
                    todo("Lorem", Completed),
                    todo("Ipsum", InProgress),
                    todo("Dolor", Pending),
                    todo("Sit", Pending),
                    todo("", Pending),
                ][..]
            ),
            "an unknown status reads pending"
        );
        let TranscriptBody::ToolCall { todos, .. } = &got[1].body else {
            panic!("a call");
        };
        assert_eq!(*todos, None);
    }

    /// The shape `src/api.ts` declares.
    #[test]
    fn a_page_serializes_in_the_webview_shape() {
        let mut t = Transcript::default();
        t.push(
            4,
            &tool_use(
                1,
                "t1",
                "TodoWrite",
                json!({"todos": [{"content": "Lorem", "status": "in_progress"}]}),
            )
            .to_string(),
        );
        t.push(5, &user(2, json!("Ipsum.")).to_string());
        t.push(6, &result(3, "t0", json!("Dolor."), true).to_string());
        let tagged = "<teammate-message teammate_id=\"lorem-mate\">\nSit.\n</teammate-message>";
        t.push(7, &user(4, json!(tagged)).to_string());
        let page = TranscriptPage {
            entries: t.entries,
            updates: t.updates,
            cursor: Some(7),
        };
        assert_eq!(
            serde_json::to_value(&page).expect("json"),
            json!({
                "entries": [
                    {"key": "4:0", "timestamp": at(1), "kind": "tool_call", "id": "t1",
                        "name": "TodoWrite", "args": "TodoWrite(1 todos)", "status": "pending",
                        "result": null, "resultTruncated": false, "resultAt": null,
                        "todos": [{"content": "Lorem", "status": "in_progress"}]},
                    {"key": "5:0", "timestamp": at(2), "kind": "user", "text": "Ipsum.",
                        "sender": null},
                    {"key": "7:0", "timestamp": at(4), "kind": "user", "text": "Sit.",
                        "sender": "lorem-mate"},
                ],
                "updates": [
                    {"id": "t0", "status": "error", "result": "Dolor.", "resultTruncated": false,
                        "resultAt": at(3)},
                ],
                "cursor": 7,
            })
        );
    }

    #[test]
    fn an_entry_is_keyed_by_its_event_and_its_place_in_the_record() {
        let mut t = Transcript::default();
        t.push(
            7,
            &assistant(
                1,
                json!([{"type": "thinking", "thinking": "", "signature": "c2ln"},
                {"type": "thinking", "thinking": "Lorem."}, {"type": "text", "text": "Ipsum."},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}}]),
            )
            .to_string(),
        );
        t.push(9, &user(2, json!("   ")).to_string());
        t.push(12, &user(3, json!("Dolor.")).to_string());
        let keys: Vec<_> = t.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            ["7:0", "7:1", "7:2", "12:0"],
            "a skipped block takes no place"
        );
    }

    #[test]
    fn a_result_whose_call_is_not_in_the_page_is_an_update() {
        use TranscriptToolStatus::*;
        let long = "x".repeat(RESULT_MAX + 1);
        let mut t = Transcript::default();
        let records = [
            tool_use(1, "t2", "Read", json!({"file_path": "/work/x"})),
            result(2, "t1", json!("Lorem."), false),
            result(3, "t2", json!("Ipsum."), true),
            result(4, "t3", json!(long), false),
            result(5, "", json!("No call."), false),
            user(
                6,
                json!([{"type": "tool_result", "content": "No id.", "is_error": false}]),
            ),
        ];
        for (i, r) in (1..).zip(&records) {
            t.push(i, &r.to_string());
        }
        assert_eq!(
            t.updates,
            [
                TranscriptResult {
                    id: "t1".into(),
                    status: Success,
                    result: "Lorem.".into(),
                    result_truncated: false,
                    result_at: Some(at(2)),
                },
                TranscriptResult {
                    id: "t3".into(),
                    status: Success,
                    result: "x".repeat(RESULT_MAX),
                    result_truncated: true,
                    result_at: Some(at(4)),
                },
            ],
            "a result with no id answers nothing"
        );
        assert_eq!(
            call(&t.entries[0]),
            (
                "t2",
                "Read(/work/x)",
                Error,
                Some("Ipsum."),
                false,
                Some(at(3).as_str())
            ),
            "a call in the page is completed in it"
        );
    }

    /// Stores `raw` as the next event of `session`'s agent `agent_id`, typed
    /// by its `type` (`user` when it is no JSON).
    fn insert(conn: &Connection, session: &str, agent_id: Option<&str>, raw: &str) {
        let kind = serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| v["type"].as_str().map(str::to_string))
            .unwrap_or_else(|| "user".into());
        conn.execute(
            "INSERT INTO events (session_id, agent_id, type, file, byte_offset, raw)
             VALUES (?1, ?2, ?3, ?1, (SELECT count(*) FROM events), ?4)",
            (session, agent_id, kind, raw),
        )
        .expect("event");
    }

    /// `page` added to `all` as the console does: each update completes the
    /// last call with its id, if any, then the entries are appended.
    fn accumulate(all: &mut Vec<TranscriptEntry>, page: TranscriptPage) {
        for done in page.updates {
            let call = all.iter_mut().rev().find_map(|e| match &mut e.body {
                TranscriptBody::ToolCall {
                    id,
                    status,
                    result,
                    result_truncated,
                    result_at,
                    ..
                } if *id == done.id => Some((status, result, result_truncated, result_at)),
                _ => None,
            });
            if let Some((status, result, truncated, at)) = call {
                *status = done.status;
                *result = Some(done.result);
                *truncated = done.result_truncated;
                *at = done.result_at;
            }
        }
        all.extend(page.entries);
    }

    /// Records whose calls are answered at every distance, together with the
    /// synthetic demo session's own lines.
    fn stepped_records() -> Vec<String> {
        let fixture = include_str!(
            "../../tests/fixtures/claude-home/projects/-work-demo-repo/5e55a0a0-0000-4000-8000-00000000000a.jsonl"
        );
        let records = [
            user(1, json!("Lorem ipsum.")),
            assistant(
                2,
                json!([{"type": "thinking", "thinking": "Dolor."}, {"type": "text", "text": "Sit."},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
                {"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/work/a"}}]),
            ),
            result(3, "t1", json!("Amet."), false),
            assistant(4, json!("Consectetur.")),
            json!({"type": "user", "isMeta": true, "timestamp": at(5),
                "message": {"role": "user", "content": "Lorem caveat."}}),
            result(6, "t2", json!("Adipiscing."), true),
            tool_use(
                7,
                "t3",
                "TodoWrite",
                json!({"todos": [{"content": "Elit", "status": "pending"}]}),
            ),
            user(
                8,
                json!([{"type": "tool_result", "tool_use_id": "t3", "content": "Ok."},
                {"type": "text", "text": "Sed do."}]),
            ),
            result(9, "t9", json!("An orphan."), false),
            tool_use(10, "t1", "Bash", json!({"command": "pwd"})),
            tool_use(11, "", "Grep", json!({"pattern": "x"})),
            result(12, "", json!("No call."), false),
            tool_use(13, "t4", "Glob", json!({"pattern": "*.rs"})),
            result(14, "t1", json!("/work"), false),
        ];
        let mut raws: Vec<String> = records.iter().map(Value::to_string).collect();
        raws.push("{not json".into());
        raws.extend(fixture.lines().map(str::to_string));
        raws.extend(include_str!("../../tests/fixtures/sender-home/projects/-work-sender/5e55c1c1-0000-4000-8000-0000000000f1.jsonl").lines().map(str::to_string));
        raws
    }

    #[test]
    fn fed_in_steps_the_pages_add_up_to_the_one_shot_read() {
        let dir = crate::tail::test_dir("transcript-steps");
        let store = super::super::Store::open_in(dir.join("strate.db"), "UTC").expect("open");
        let conn = super::super::Store::open_reader(dir.join("strate.db")).expect("reader");
        let raws = stepped_records();
        let mut updated = false;
        // Agent row `step` reads its records `step` at a time.
        for step in 1..=raws.len() {
            let session = format!("s{step}");
            store
                .conn
                .execute("INSERT INTO sessions (session_id) VALUES (?1)", [&session])
                .expect("session");
            store
                .conn
                .execute(
                    "INSERT INTO agents (id, session_id, agent_id, kind)
                     VALUES (?1, ?2, NULL, 'orchestrator')",
                    (step as i64, &session),
                )
                .expect("agent");
            let mut all = Vec::new();
            let mut cursor = None;
            for chunk in raws.chunks(step) {
                for raw in chunk {
                    insert(&store.conn, &session, None, raw);
                }
                // Another agent's record lands between reads.
                insert(
                    &store.conn,
                    &session,
                    Some("w"),
                    &user(1, json!("Not its.")).to_string(),
                );
                let page = agent_transcript(&conn, step as i64, cursor).expect("page");
                updated |= !page.updates.is_empty();
                cursor = page.cursor;
                accumulate(&mut all, page);
                let again = agent_transcript(&conn, step as i64, cursor).expect("again");
                assert_eq!(
                    again,
                    TranscriptPage {
                        entries: vec![],
                        updates: vec![],
                        cursor
                    },
                    "nothing new is nothing"
                );
            }
            let whole = agent_transcript(&conn, step as i64, None).expect("whole");
            assert_eq!(all, whole.entries, "{step} at a time");
            assert_eq!(cursor, whole.cursor);
        }
        assert!(updated, "some result completed an earlier page's call");
        drop((conn, store));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    /// A store holding agent rows 1 (session s's orchestrator) and 2 (its
    /// subagent w), records of each, and records neither reads.
    #[test]
    fn an_agents_transcript_is_its_own_records_in_stored_order() {
        let dir = crate::tail::test_dir("transcript-own-records");
        let store = super::super::Store::open_in(dir.join("strate.db"), "UTC").expect("open");
        let line = |v: Value| v.to_string().replace('\'', "''");
        // Stored out of time order: the subagent's record is between two of
        // the orchestrator's, and an attachment record is no user record.
        let sql = format!(
            "INSERT INTO sessions (session_id) VALUES ('s'), ('o');
             INSERT INTO agents (id, session_id, agent_id, kind) VALUES
                 (1, 's', NULL, 'orchestrator'), (2, 's', 'w', 'subagent'),
                 (3, 'o', NULL, 'orchestrator');
             INSERT INTO events (session_id, agent_id, type, file, byte_offset, raw) VALUES
                 ('s', NULL, 'user', 's', 0, '{}'),
                 ('s', 'w', 'user', 'w', 0, '{}'),
                 ('s', NULL, 'assistant', 's', 1, '{}'),
                 ('s', NULL, 'attachment', 's', 2, '{}'),
                 ('o', NULL, 'user', 'o', 0, '{}');",
            line(user(1, json!("Lorem."))),
            line(user(2, json!("Ipsum."))),
            line(assistant(3, json!([{"type": "text", "text": "Dolor."}]))),
            line(user(4, json!("An attachment's text."))),
            line(user(5, json!("Another session's."))),
        );
        store.conn.execute_batch(&sql).expect("seed");
        let conn = super::super::Store::open_reader(dir.join("strate.db")).expect("reader");
        let keyed = |key: &str, e: TranscriptEntry| TranscriptEntry {
            key: key.into(),
            ..e
        };
        let page = |entries, cursor| TranscriptPage {
            entries,
            updates: vec![],
            cursor,
        };
        let dolor = entry(
            3,
            TranscriptBody::Text {
                text: "Dolor.".into(),
            },
        );
        assert_eq!(
            agent_transcript(&conn, 1, None).expect("orchestrator"),
            page(
                vec![keyed("1:0", said(1, "Lorem.")), keyed("3:0", dolor.clone())],
                Some(3)
            )
        );
        assert_eq!(
            agent_transcript(&conn, 1, Some(1)).expect("after the first"),
            page(vec![keyed("3:0", dolor)], Some(3)),
        );
        assert_eq!(
            agent_transcript(&conn, 1, Some(3)).expect("after the last"),
            page(vec![], Some(3)),
            "nothing new keeps the cursor"
        );
        assert_eq!(
            agent_transcript(&conn, 2, None).expect("subagent"),
            page(vec![keyed("2:0", said(2, "Ipsum."))], Some(2))
        );
        assert_eq!(
            agent_transcript(&conn, 99, None).expect("unknown"),
            page(vec![], None)
        );
        assert_eq!(
            super::super::query_plan(&conn, TRANSCRIPT_SQL),
            [
                "SEARCH a USING INTEGER PRIMARY KEY (rowid=?)",
                "SEARCH e USING INDEX events_agent (session_id=? AND agent_id=? AND rowid>?)",
            ],
            "the index seeks past the cursor and yields rowid order: no sort"
        );
        drop((conn, store));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
