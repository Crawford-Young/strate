-- v23: every lane (#108). A worker's finished and background state, and
-- the `launch` edge from a `claude --bg` command to the session it started.
-- Store::open fills them for a store from before this (meta `model`, v2);
-- ingest and discovery keep them current (store/model.rs).

-- Meta.json `finished`: 1 once the subagent's meta says it finished. A
-- discovery pass (after the initial scan, or when a not-yet-linked
-- subagent transcript gets records) and a batch on a subagent's transcript
-- while it is not re-read it; a finished worker with no notification gets
-- it at the next such pass or at app start. It never goes back.
ALTER TABLE agents ADD COLUMN finished INTEGER CHECK (finished IN (0, 1));
-- A subagent's dispatch tool_result: `async_launched` for a background
-- dispatch's immediate launch (toolUseResult.status), else `answered`.
-- NULL while none is stored.
ALTER TABLE agents ADD COLUMN dispatch_result TEXT
    CHECK (dispatch_result IN ('answered', 'async_launched'));

-- Each `claude --bg` invocation in a Bash or PowerShell tool_use
-- (discovery::launches), `seq` its place in the command, stored with the
-- tool_result's text once it arrives. `agent` is the agent whose transcript
-- holds the tool_use.
CREATE TABLE launches (
    id INTEGER PRIMARY KEY,
    agent INTEGER NOT NULL REFERENCES agents (id),
    tool_use_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    at TEXT,
    name TEXT,
    resume_id TEXT,
    result TEXT,
    UNIQUE (tool_use_id, seq)
) STRICT;

-- Spawn edges gain `launch` (a background session's orchestrator under the
-- agent whose command started it). SQLite cannot alter a CHECK, so the
-- table is rebuilt, and the views over it go first and come back as they
-- were, but for `launch` in spawn_parents.
DROP VIEW charge_costs;
DROP VIEW agent_charges;
DROP VIEW spawn_parents;
CREATE TABLE edges_v23 (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL
        CHECK (kind IN ('dispatch', 'teammate', 'continuation', 'message', 'launch')),
    parent INTEGER REFERENCES agents (id),
    child INTEGER NOT NULL REFERENCES agents (id),
    tool_use_id TEXT,
    team_name TEXT,
    team_dir TEXT,
    CHECK (kind NOT IN ('dispatch', 'message', 'launch') OR parent IS NOT NULL),
    UNIQUE (kind, child)
) STRICT;
INSERT INTO edges_v23 (id, kind, parent, child, tool_use_id, team_name, team_dir)
SELECT id, kind, parent, child, tool_use_id, team_name, team_dir FROM edges;
DROP TABLE edges;
ALTER TABLE edges_v23 RENAME TO edges;
CREATE INDEX edges_child ON edges (child);
-- A tool_result finds the dispatch it answers.
CREATE INDEX edges_tool_use ON edges (tool_use_id) WHERE tool_use_id IS NOT NULL;

-- Each agent's spawn parent: its dispatch, else teammate, else launch,
-- else message edge. A `/clear` continuation is not a spawn.
CREATE VIEW spawn_parents AS
SELECT e.child, e.parent, e.kind
FROM edges AS e
WHERE e.parent IS NOT NULL AND e.kind IN ('dispatch', 'teammate', 'launch', 'message')
    AND NOT EXISTS (SELECT 1 FROM edges AS o
        WHERE o.child = e.child AND o.parent IS NOT NULL
            AND o.kind IN ('dispatch', 'teammate', 'launch', 'message')
            AND (o.kind = 'dispatch' AND e.kind <> 'dispatch'
                OR o.kind = 'teammate' AND e.kind IN ('launch', 'message')
                OR o.kind = 'launch' AND e.kind = 'message'));

-- As in v21.
CREATE VIEW agent_charges AS
WITH RECURSIVE up (agent, at, depth, seen) AS (
    SELECT id, id, 0, ',' || id || ',' FROM agents
    UNION ALL
    SELECT up.agent, p.parent, up.depth + 1, up.seen || p.parent || ','
    FROM up
    JOIN agents AS a ON a.id = up.at AND a.own_charge IS NULL
    JOIN spawn_parents AS p ON p.child = up.at
    WHERE up.depth < 32 AND instr(up.seen, ',' || p.parent || ',') = 0
),
found AS (
    SELECT up.agent, up.depth, h.id AS holder, h.own_charge AS charge,
        h.own_charge_source AS source
    FROM up JOIN agents AS h ON h.id = up.at
    WHERE h.own_charge IS NOT NULL
)
SELECT a.id AS agent, f.charge, f.holder,
    CASE WHEN f.charge IS NULL THEN 'unlinked' WHEN f.depth = 0 THEN f.source ELSE 'parent' END
        AS source
FROM agents AS a LEFT JOIN found AS f ON f.agent = a.id;

-- As in v22.
CREATE VIEW charge_costs AS
SELECT x.charge, sum(r.cost_usd) AS cost_usd, count(*) - count(r.cost_usd) AS unpriced
FROM requests AS r
LEFT JOIN agents AS a ON a.session_id = r.session_id
    AND ifnull(a.agent_id, '') = ifnull(r.agent_id, '')
LEFT JOIN agent_charges AS x ON x.agent = a.id
GROUP BY x.charge;
