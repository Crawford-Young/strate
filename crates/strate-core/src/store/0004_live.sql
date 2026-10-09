-- v4: live state (#15). The `claude agents --json --all` registry and the
-- optional http hook events.

-- One row per registry entry ever seen, keyed by RegistryEntry::key. An
-- entry whose session has no transcript yet is listed here (and gets a stub
-- session); one with no sessionId is listed here only.
CREATE TABLE registry (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    -- interactive | background, or a kind this build does not know.
    kind TEXT NOT NULL,
    session_id TEXT,
    pid INTEGER,
    -- A background session's `id`.
    bg_id TEXT,
    name TEXT,
    cwd TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    -- A live session's busy | waiting | idle; a background one's state.
    status TEXT,
    waiting_for TEXT,
    state TEXT,
    first_seen_ms INTEGER NOT NULL,
    -- The poll that last changed the entry; once gone, the last poll that
    -- listed it.
    last_seen_ms INTEGER NOT NULL,
    gone INTEGER NOT NULL DEFAULT 0 CHECK (gone IN (0, 1))
) STRICT;
CREATE INDEX registry_session ON registry (session_id) WHERE session_id IS NOT NULL;

-- From hook events: running (SubagentStart), needs_you (PermissionRequest,
-- cleared by a later transcript record or, on an orchestrator, by the
-- registry), idle (TeammateIdle), done (SubagentStop).
ALTER TABLE agents ADD COLUMN hook_state TEXT
    CHECK (hook_state IN ('running', 'needs_you', 'idle', 'done'));
-- PermissionRequest's tool_name.
ALTER TABLE agents ADD COLUMN hook_waiting_for TEXT;
-- Receipt time (unix ms) of the hook that set hook_state.
ALTER TABLE agents ADD COLUMN hook_at_ms INTEGER;

-- Everything waiting on the user: live registry entries with status
-- waiting and a waitingFor (agent NULL while it has no sessionId), and
-- agents a PermissionRequest hook left needing you.
CREATE VIEW needs_you AS
SELECT 'registry' AS source, r.id AS registry, a.id AS agent, r.session_id,
    NULL AS agent_id, r.waiting_for, r.last_seen_ms AS since_ms
FROM registry AS r
LEFT JOIN agents AS a ON a.session_id = r.session_id AND a.agent_id IS NULL
WHERE r.gone = 0 AND r.status = 'waiting' AND r.waiting_for IS NOT NULL
UNION ALL
SELECT 'hook', NULL, a.id, a.session_id, a.agent_id, a.hook_waiting_for, a.hook_at_ms
FROM agents AS a
WHERE a.hook_state = 'needs_you';
