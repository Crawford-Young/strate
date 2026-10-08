-- v1: the initial schema. Append-only: later changes are new migrations.

-- Orchestrator tabs: sessions sharing a `<repo>-<issue>` name. Filled by #13.
CREATE TABLE workstreams (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    -- 1 when `name` is the cwd/branch fallback, not a user-set name.
    fallback INTEGER NOT NULL DEFAULT 0 CHECK (fallback IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    project_dir TEXT,
    -- The session transcript; NULL while no transcript has been seen.
    path TEXT,
    -- 1 while only subagent transcripts of this session have been seen.
    stub INTEGER NOT NULL DEFAULT 0 CHECK (stub IN (0, 1)),
    workstream_id INTEGER REFERENCES workstreams (id) ON DELETE SET NULL,
    -- Rollups, computed by #14.
    cost_usd REAL,
    run_ms INTEGER
) STRICT;

-- One node per orchestrator (agent_id NULL), subagent or teammate.
CREATE TABLE agents (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (session_id),
    agent_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('orchestrator', 'subagent', 'teammate')),
    path TEXT,
    -- From the subagent's .meta.json.
    agent_type TEXT,
    description TEXT,
    spawn_depth INTEGER,
    team_name TEXT,
    -- Why a subagent has no parent edge: no_meta | no_tool_use_id | tool_use_not_found.
    unresolved TEXT,
    -- Actual values: meta first, then the latest transcript record wins.
    model TEXT,
    effort TEXT,
    cwd TEXT,
    git_branch TEXT,
    version TEXT,
    -- Latest custom-title / agent-name (/rename), or a teammate's meta name.
    name TEXT,
    -- From the live registry, captured by #15.
    registry_name TEXT,
    registry_state TEXT,
    -- Rollups, computed by #14.
    cost_usd REAL,
    run_ms INTEGER,
    wait_ms INTEGER,
    CHECK ((kind = 'orchestrator') = (agent_id IS NULL))
) STRICT;
CREATE UNIQUE INDEX agents_key ON agents (session_id, ifnull(agent_id, ''));

CREATE TABLE edges (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('dispatch', 'teammate', 'continuation')),
    parent INTEGER REFERENCES agents (id),
    child INTEGER NOT NULL REFERENCES agents (id),
    tool_use_id TEXT,
    team_name TEXT,
    team_dir TEXT,
    CHECK (kind <> 'dispatch' OR (parent IS NOT NULL AND tool_use_id IS NOT NULL)),
    UNIQUE (kind, child)
) STRICT;

-- Where each transcript resumes. Written in the same transaction as the
-- events of the batch that ends there.
CREATE TABLE offsets (
    path TEXT PRIMARY KEY,
    byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
    -- u64 volume stored by bit pattern; u128 file index as decimal text.
    volume INTEGER NOT NULL,
    file_index TEXT NOT NULL
) STRICT;

-- One row per transcript record, raw JSON kept for scrollback and search.
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (session_id),
    agent_id TEXT,
    type TEXT,
    subtype TEXT,
    uuid TEXT,
    parent_uuid TEXT,
    request_id TEXT,
    timestamp TEXT,
    file TEXT NOT NULL,
    byte_offset INTEGER NOT NULL,
    raw TEXT NOT NULL
) STRICT;
-- Resumed sessions replay records into a new file: a uuid is stored once.
CREATE UNIQUE INDEX events_uuid ON events (uuid) WHERE uuid IS NOT NULL;
CREATE UNIQUE INDEX events_position ON events (file, byte_offset) WHERE uuid IS NULL;
CREATE INDEX events_file ON events (file);
CREATE INDEX events_agent ON events (session_id, agent_id);
CREATE INDEX events_request ON events (request_id) WHERE request_id IS NOT NULL;
CREATE INDEX events_timestamp ON events (timestamp);

CREATE TABLE github_links (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (session_id),
    pr_number INTEGER,
    pr_url TEXT NOT NULL,
    pr_repository TEXT,
    linked_at TEXT,
    UNIQUE (session_id, pr_url)
) STRICT;

-- A launch (parent_agent NULL) or dispatch request, correlated to the agent
-- it produces. In the model from v1, enabled in v2.
CREATE TABLE intents (
    id INTEGER PRIMARY KEY,
    parent_agent INTEGER REFERENCES agents (id),
    cwd TEXT,
    model TEXT,
    effort TEXT,
    cost_cap_usd REAL CHECK (cost_cap_usd > 0),
    time_cap_ms INTEGER CHECK (time_cap_ms > 0),
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    correlated_agent INTEGER REFERENCES agents (id)
) STRICT;

-- The orchestrator's row holds its session's defaults; any other agent's
-- row holds that agent's overrides. NULL inherits.
CREATE TABLE gear_settings (
    agent INTEGER PRIMARY KEY REFERENCES agents (id),
    nudge_pct INTEGER CHECK (nudge_pct BETWEEN 0 AND 100),
    hard_stop_pct INTEGER CHECK (hard_stop_pct BETWEEN 0 AND 100),
    cost_warn_pct INTEGER CHECK (cost_warn_pct BETWEEN 0 AND 100),
    cost_cap_usd REAL CHECK (cost_cap_usd > 0),
    time_cap_ms INTEGER CHECK (time_cap_ms > 0),
    cwd TEXT,
    model TEXT,
    effort TEXT
) STRICT;

-- Effective gear per agent: own row, else the orchestrator's, else the
-- built-in nudge 80 / hard stop 95.
CREATE VIEW gear_effective AS
SELECT
    a.id AS agent,
    a.session_id,
    coalesce(own.nudge_pct, orch.nudge_pct, 80) AS nudge_pct,
    coalesce(own.hard_stop_pct, orch.hard_stop_pct, 95) AS hard_stop_pct,
    coalesce(own.cost_warn_pct, orch.cost_warn_pct) AS cost_warn_pct,
    coalesce(own.cost_cap_usd, orch.cost_cap_usd) AS cost_cap_usd,
    coalesce(own.time_cap_ms, orch.time_cap_ms) AS time_cap_ms,
    coalesce(own.cwd, orch.cwd) AS cwd,
    coalesce(own.model, orch.model) AS model,
    coalesce(own.effort, orch.effort) AS effort
FROM agents AS a
LEFT JOIN agents AS o ON o.session_id = a.session_id AND o.agent_id IS NULL
LEFT JOIN gear_settings AS own ON own.agent = a.id
LEFT JOIN gear_settings AS orch ON orch.agent = o.id;

-- Hard stop stays above nudge for every agent of the session, overrides
-- and inherited defaults alike.
CREATE TRIGGER gear_order_on_insert AFTER INSERT ON gear_settings
WHEN EXISTS (
    SELECT 1 FROM gear_effective
    WHERE session_id = (SELECT session_id FROM agents WHERE id = NEW.agent)
      AND hard_stop_pct <= nudge_pct
)
BEGIN
    SELECT RAISE(ABORT, 'gear: hard stop must be above nudge');
END;

CREATE TRIGGER gear_order_on_update AFTER UPDATE ON gear_settings
WHEN EXISTS (
    SELECT 1 FROM gear_effective
    WHERE session_id = (SELECT session_id FROM agents WHERE id = NEW.agent)
      AND hard_stop_pct <= nudge_pct
)
BEGIN
    SELECT RAISE(ABORT, 'gear: hard stop must be above nudge');
END;
