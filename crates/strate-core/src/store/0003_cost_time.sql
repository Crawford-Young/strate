-- v3: cost and time (#14). The per-record fields accounting reads, priced
-- requests, rollups and per-agent context. Store::open fills the new event
-- columns from `raw` and computes every rollup after this runs.

-- On every assistant record: requestId, else message.id, else uuid.
ALTER TABLE events ADD COLUMN request_key TEXT;
-- A request record (an assistant record with usage and a real model, never
-- `<synthetic>`): its model, message.usage JSON and output tokens.
ALTER TABLE events ADD COLUMN model TEXT;
ALTER TABLE events ADD COLUMN usage TEXT;
ALTER TABLE events ADD COLUMN output_tokens INTEGER;
-- The request record's $ at the vendored prices; NULL when its model has
-- no price (unpriced, never $0).
ALTER TABLE events ADD COLUMN cost_usd REAL;
-- Timing: tool_use blocks as [[id, name], ...], tool_result ids as
-- [id, ...], and on a prompt (a user record with no tool result, not
-- isMeta) 1 when a person typed it, 0 for a task notification.
ALTER TABLE events ADD COLUMN tool_uses TEXT;
ALTER TABLE events ADD COLUMN tool_results TEXT;
ALTER TABLE events ADD COLUMN prompt INTEGER;
CREATE INDEX events_request_key ON events (request_key) WHERE usage IS NOT NULL;

-- Rollups. cost_usd sums priced requests (NULL with none); unpriced counts
-- requests whose model has no price. run_ms is wall time; a session's
-- active_ms drops idle gaps over 10 min, wait_ms is time waiting on the
-- user, agent_ms sums its subagents' run_ms.
ALTER TABLE sessions ADD COLUMN unpriced INTEGER;
ALTER TABLE sessions ADD COLUMN active_ms INTEGER;
ALTER TABLE sessions ADD COLUMN wait_ms INTEGER;
ALTER TABLE sessions ADD COLUMN agent_ms INTEGER;
ALTER TABLE agents ADD COLUMN unpriced INTEGER;
-- The latest request's depth (input + cache read + cache write), and that
-- over its model's window; NULL % for a model with no known window.
ALTER TABLE agents ADD COLUMN context_tokens INTEGER;
ALTER TABLE agents ADD COLUMN context_pct REAL;

-- Store facts. `prices`: the price list the stored $ were computed with.
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

-- One row per API request: of the request records sharing a key (streaming
-- partials, replays), the one with the most output; ties go to the earliest.
CREATE VIEW requests AS
SELECT e.id, e.session_id, e.agent_id, e.request_key, e.timestamp, e.model, e.usage,
    e.output_tokens, e.cost_usd
FROM events AS e
WHERE e.usage IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM events AS o
    WHERE o.request_key = e.request_key AND o.usage IS NOT NULL AND o.id <> e.id
      AND (o.output_tokens > e.output_tokens OR (o.output_tokens = e.output_tokens
          AND (ifnull(o.timestamp, ''), o.file, o.byte_offset)
              < (ifnull(e.timestamp, ''), e.file, e.byte_offset)))
);

-- $ per (merge-resolved) workstream.
CREATE VIEW workstream_costs AS
SELECT w.workstream, sum(r.cost_usd) AS cost_usd, count(*) - count(r.cost_usd) AS unpriced
FROM requests AS r JOIN event_workstreams AS w ON w.event = r.id
WHERE w.workstream IS NOT NULL
GROUP BY w.workstream;
