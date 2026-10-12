-- v6: audit cells (#26). One row per (session, agent, window, local day,
-- model): the slice of a timeline's time and requests the Audit rollups sum.
-- Store::open computes every cell after this runs.

CREATE TABLE audit_cells (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (session_id),
    -- NULL on the orchestrator, whose cells carry the session's timeline
    -- (subagent activity counts as activity there); a subagent's carry its
    -- own run.
    agent_id TEXT,
    -- The /rename window: an orchestrator's time by when each window
    -- started, its requests by their place in the transcript; a subagent's
    -- everything by the window it started in. NULL when there is none.
    segment_id INTEGER REFERENCES segments (id) ON DELETE SET NULL,
    -- Local calendar day (YYYY-MM-DD) in the zone under meta `tz`; NULL for
    -- a request with no usable timestamp, kept so the sums stay exact.
    day TEXT,
    -- Time, clipped at local midnight and window starts. It does not split by
    -- model: it sits on the row of the model with the most requests in the
    -- (agent, window, day) (ties: the latest), and is 0 on its other rows.
    wall_ms INTEGER NOT NULL,
    active_ms INTEGER NOT NULL,
    wait_ms INTEGER NOT NULL,
    -- The requests of this model, each counted on the day and window of its
    -- own record.
    -- cost_usd is NULL with no priced request, unpriced counts the rest.
    cost_usd REAL,
    unpriced INTEGER NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    -- cache_creation_input_tokens: the whole write.
    cache_write_tokens INTEGER NOT NULL,
    -- NULL on the row of an (agent, window, day) with time but no requests.
    model TEXT
) STRICT;
CREATE INDEX audit_cells_session ON audit_cells (session_id, agent_id);
CREATE INDEX audit_cells_day ON audit_cells (day);
