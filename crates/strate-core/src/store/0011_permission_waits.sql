-- v11: permission waits (#52), and a gone session never needs you (#64).
-- Each time an agent waited on a permission prompt: from its
-- PermissionRequest hook to its next user or assistant record, any later
-- hook on it, or (on an orchestrator) the registry no longer listing it
-- waiting, or no longer listing it at all. Recorded only while the hook
-- receiver runs.

CREATE TABLE permission_waits (
    id INTEGER PRIMARY KEY,
    agent INTEGER NOT NULL REFERENCES agents (id),
    -- The first request's tool_name: a repeat request before the agent
    -- moved on is the same wait.
    tool_name TEXT,
    -- Receipt time (unix ms) of the request.
    start_ms INTEGER NOT NULL,
    -- NULL while the agent still waits (its hook_state is needs_you).
    end_ms INTEGER CHECK (end_ms >= start_ms)
) STRICT;
-- At most one open wait per agent.
CREATE UNIQUE INDEX permission_waits_open ON permission_waits (agent) WHERE end_ms IS NULL;

-- An orchestrator whose session the registry lists only as gone exited
-- mid-prompt: it needs nothing.
UPDATE agents SET hook_state = NULL, hook_waiting_for = NULL
WHERE agent_id IS NULL AND hook_state = 'needs_you'
    AND EXISTS (SELECT 1 FROM registry AS r WHERE r.session_id = agents.session_id)
    AND NOT EXISTS (
        SELECT 1 FROM registry AS r WHERE r.session_id = agents.session_id AND r.gone = 0);

-- Requests still unanswered when this ran: open waits.
INSERT INTO permission_waits (agent, tool_name, start_ms)
SELECT id, hook_waiting_for, hook_at_ms FROM agents
WHERE hook_state = 'needs_you' AND hook_at_ms IS NOT NULL;

-- As in v4, but a hook's needs-you is listed only while its session is not
-- gone: some registry row of it live, or none listing it at all.
DROP VIEW needs_you;
CREATE VIEW needs_you AS
SELECT 'registry' AS source, r.id AS registry, a.id AS agent, r.session_id,
    NULL AS agent_id, r.waiting_for, r.last_seen_ms AS since_ms
FROM registry AS r
LEFT JOIN agents AS a ON a.session_id = r.session_id AND a.agent_id IS NULL
WHERE r.gone = 0 AND r.status = 'waiting' AND r.waiting_for IS NOT NULL
UNION ALL
SELECT 'hook', NULL, a.id, a.session_id, a.agent_id, a.hook_waiting_for, a.hook_at_ms
FROM agents AS a
WHERE a.hook_state = 'needs_you'
    AND (EXISTS (SELECT 1 FROM registry AS r WHERE r.session_id = a.session_id AND r.gone = 0)
        OR NOT EXISTS (SELECT 1 FROM registry AS r WHERE r.session_id = a.session_id));
