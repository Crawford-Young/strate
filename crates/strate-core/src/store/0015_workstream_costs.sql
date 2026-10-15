-- v15: workstream $ from an index (#79). The request columns sit past `raw`
-- on disk (v3), so `requests` read every request record, and
-- `workstream_costs` read each one's row a second time to find its
-- segment (`event_workstreams`). This index holds what both read; its last
-- column only makes it covering, since a partial index's own WHERE term
-- still counts as a column read.
DROP INDEX events_request_key;
CREATE INDEX events_requests ON events (
    request_key, output_tokens, timestamp, file, byte_offset, cost_usd, session_id, agent_id,
    usage IS NOT NULL
) WHERE usage IS NOT NULL;

-- As in v3, plus each request record's byte_offset.
DROP VIEW requests;
CREATE VIEW requests AS
SELECT e.id, e.session_id, e.agent_id, e.request_key, e.timestamp, e.model, e.usage,
    e.output_tokens, e.cost_usd, e.byte_offset
FROM events AS e
WHERE e.usage IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM events AS o
    WHERE o.request_key = e.request_key AND o.usage IS NOT NULL AND o.id <> e.id
      AND (o.output_tokens > e.output_tokens OR (o.output_tokens = e.output_tokens
          AND (ifnull(o.timestamp, ''), o.file, o.byte_offset)
              < (ifnull(e.timestamp, ''), e.file, e.byte_offset)))
);

-- $ per (merge-resolved) workstream. Each request finds its segment as
-- `event_workstreams` finds an event's, from the request's own columns.
DROP VIEW workstream_costs;
CREATE VIEW workstream_costs AS
SELECT w.root AS workstream, sum(x.cost_usd) AS cost_usd,
    count(*) - count(x.cost_usd) AS unpriced
FROM (
    SELECT r.cost_usd,
        CASE WHEN r.agent_id IS NULL THEN (
            SELECT s.id FROM segments AS s
            WHERE s.session_id = r.session_id AND s.start_offset <= r.byte_offset
            ORDER BY s.start_offset DESC LIMIT 1
        ) ELSE (
            SELECT a.segment_id FROM agents AS a
            WHERE a.session_id = r.session_id AND a.agent_id = r.agent_id
        ) END AS segment
    FROM requests AS r
) AS x
JOIN segments AS s ON s.id = x.segment
JOIN workstream_roots AS w ON w.id = s.workstream_id
GROUP BY w.root;
