-- v21: the generic agent model (#107). Every agent has a lane, a role, a
-- charge and a parent. Store::open builds the model of a store from before
-- this (meta `model`); ingest, discovery, the registry and the GitHub cache
-- keep it current (store/model.rs).

-- What an agent works on, one row per charge key `<kind>_<target>`
-- (crate::charge). Rows are shared and never removed.
CREATE TABLE charges (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('repo', 'epic', 'issue', 'pr', 'topic')),
    repo TEXT,
    number INTEGER,
    topic TEXT
) STRICT;

-- The role and charge the agent's name parses to, and its own charge: its
-- git branch's PR's closing issue, else that name charge. Recomputed from
-- stored rows whenever an input changes.
ALTER TABLE agents ADD COLUMN role TEXT;
ALTER TABLE agents ADD COLUMN name_charge INTEGER REFERENCES charges (id);
ALTER TABLE agents ADD COLUMN own_charge INTEGER REFERENCES charges (id);
ALTER TABLE agents ADD COLUMN own_charge_source TEXT
    CHECK (own_charge_source IN ('branch', 'name'));

-- A PR's head branch (GraphQL headRefName); NULL until the repo's next refresh.
ALTER TABLE gh_prs ADD COLUMN head_ref TEXT;
CREATE INDEX gh_prs_head_ref ON gh_prs (head_ref) WHERE head_ref IS NOT NULL;

-- Spawn edges gain `message` (a session's orchestrator under its first
-- messager, rebuilt from the events), and a dispatch may lack its
-- tool_use_id when meta `parentAgentId` alone names its parent. SQLite
-- cannot alter a CHECK, so the table is rebuilt; nothing references it and
-- no view reads it, so the rename leaves the schema whole.
CREATE TABLE edges_v21 (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('dispatch', 'teammate', 'continuation', 'message')),
    parent INTEGER REFERENCES agents (id),
    child INTEGER NOT NULL REFERENCES agents (id),
    tool_use_id TEXT,
    team_name TEXT,
    team_dir TEXT,
    CHECK (kind NOT IN ('dispatch', 'message') OR parent IS NOT NULL),
    UNIQUE (kind, child)
) STRICT;
INSERT INTO edges_v21 (id, kind, parent, child, tool_use_id, team_name, team_dir)
SELECT id, kind, parent, child, tool_use_id, team_name, team_dir FROM edges;
DROP TABLE edges;
ALTER TABLE edges_v21 RENAME TO edges;
CREATE INDEX edges_child ON edges (child);

-- Each agent's spawn parent: its dispatch, else teammate, else message edge.
-- A `/clear` continuation is not a spawn.
CREATE VIEW spawn_parents AS
SELECT e.child, e.parent, e.kind
FROM edges AS e
WHERE e.parent IS NOT NULL AND e.kind IN ('dispatch', 'teammate', 'message')
    AND NOT EXISTS (SELECT 1 FROM edges AS o
        WHERE o.child = e.child AND o.parent IS NOT NULL
            AND o.kind IN ('dispatch', 'teammate', 'message')
            AND (o.kind = 'dispatch' AND e.kind <> 'dispatch'
                OR o.kind = 'teammate' AND e.kind = 'message'));

-- Each agent's effective charge: its own, else its nearest spawn
-- ancestor's own (`holder` is that agent), else none (Unlinked). The walk
-- stops at 32 levels and never revisits an agent.
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

-- Each agent's lane: a subagent or teammate is `subagent`; an orchestrator
-- is `background-session` when a background registry row names a session
-- of its `/clear` chain (one process root), else `interactive`. `cloud`
-- has no producer yet (#108).
CREATE VIEW agent_lanes AS
SELECT a.id AS agent,
    CASE
        WHEN a.kind <> 'orchestrator' THEN 'subagent'
        WHEN EXISTS (SELECT 1 FROM registry AS r
            JOIN sessions AS rs ON rs.session_id = r.session_id
            JOIN sessions AS s ON s.session_id = a.session_id
            WHERE r.kind = 'background'
                AND ifnull(rs.process_root, rs.session_id) = ifnull(s.process_root, s.session_id))
        THEN 'background-session'
        ELSE 'interactive'
    END AS lane
FROM agents AS a;
