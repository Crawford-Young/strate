-- v2: workstream grouping (#13). Name segments, manual merges, continuation
-- links, and the per-record fields grouping reads.

-- A manual merge: this workstream reads as the one it points at, through
-- chains (see workstream_roots). Regrouping never touches it.
ALTER TABLE workstreams ADD COLUMN merged_into INTEGER REFERENCES workstreams (id);
CREATE UNIQUE INDEX workstreams_key ON workstreams (name, fallback);

-- The process root: the snake_case `session_id` on the session's records,
-- shared by every file one process writes across /clear. Continuation edges
-- link a root's sessions in `started_at` (first record time) order.
ALTER TABLE sessions ADD COLUMN process_root TEXT;
ALTER TABLE sessions ADD COLUMN started_at TEXT;
CREATE INDEX sessions_process_root ON sessions (process_root) WHERE process_root IS NOT NULL;

ALTER TABLE events ADD COLUMN cwd TEXT;
ALTER TABLE events ADD COLUMN git_branch TEXT;
-- A custom-title record's name: written by /rename, carried into the next
-- file by /clear.
ALTER TABLE events ADD COLUMN custom_title TEXT;

-- A run of one session transcript under one name. It covers the session's
-- orchestrator events from start_offset to the next segment's start.
CREATE TABLE segments (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (session_id),
    start_offset INTEGER NOT NULL CHECK (start_offset >= 0),
    -- The segment's earliest record time; NULL until a timestamped record.
    started_at TEXT,
    workstream_id INTEGER NOT NULL REFERENCES workstreams (id),
    UNIQUE (session_id, start_offset)
) STRICT;
CREATE INDEX segments_workstream ON segments (workstream_id);

-- The segment live when a subagent or teammate started; NULL on
-- orchestrators, whose events map by offset instead.
ALTER TABLE agents ADD COLUMN segment_id INTEGER REFERENCES segments (id) ON DELETE SET NULL;

-- Backfill what v1 stored only in `raw`; Store::open regroups after.
UPDATE events SET
    cwd = CASE json_type(raw, '$.cwd') WHEN 'text' THEN json_extract(raw, '$.cwd') END,
    git_branch = CASE json_type(raw, '$.gitBranch') WHEN 'text' THEN json_extract(raw, '$.gitBranch') END,
    custom_title = CASE WHEN type = 'custom-title' AND json_type(raw, '$.customTitle') = 'text'
        THEN json_extract(raw, '$.customTitle') END;
UPDATE sessions SET process_root = (
    SELECT json_extract(e.raw, '$.session_id') FROM events AS e
    WHERE e.session_id = sessions.session_id AND e.agent_id IS NULL
      AND json_type(e.raw, '$.session_id') = 'text'
    ORDER BY e.byte_offset LIMIT 1
);

-- Each workstream and the one it reads as after manual merges.
CREATE VIEW workstream_roots AS
WITH RECURSIVE r (id, root) AS (
    SELECT id, id FROM workstreams WHERE merged_into IS NULL
    UNION ALL
    SELECT w.id, r.root FROM workstreams AS w JOIN r ON w.merged_into = r.id
)
SELECT id, root FROM r;

-- The segment and merge-resolved workstream each event counts toward: an
-- orchestrator record by its place in the transcript, any other record by
-- its agent's segment.
CREATE VIEW event_workstreams AS
SELECT x.event, x.segment, r.root AS workstream
FROM (
    SELECT e.id AS event,
        CASE WHEN e.agent_id IS NULL THEN (
            SELECT s.id FROM segments AS s
            WHERE s.session_id = e.session_id AND s.start_offset <= e.byte_offset
            ORDER BY s.start_offset DESC LIMIT 1
        ) ELSE (
            SELECT a.segment_id FROM agents AS a
            WHERE a.session_id = e.session_id AND a.agent_id = e.agent_id
        ) END AS segment
    FROM events AS e
) AS x
LEFT JOIN segments AS s ON s.id = x.segment
LEFT JOIN workstream_roots AS r ON r.id = s.workstream_id;
