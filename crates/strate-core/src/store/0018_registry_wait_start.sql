-- v18: a registry wait's start (#84). `last_seen_ms` is the poll that last
-- changed the entry, so a change mid-wait, or a restarted strate listing
-- it again, moved the wait's start, and it no longer paired with its
-- hook's request as one stop.

-- The poll that first saw the entry waiting on the user (status `waiting`,
-- or a background session `blocked`), kept while it stays so; NULL
-- otherwise.
ALTER TABLE registry ADD COLUMN waiting_since_ms INTEGER;

-- A row already waiting has no better start than its last change.
UPDATE registry SET waiting_since_ms = last_seen_ms
WHERE status IS 'waiting' OR state IS 'blocked';

-- As in v13, with a registry wait since its start.
DROP VIEW needs_you;
CREATE VIEW needs_you AS
SELECT 'registry' AS source, r.id AS registry, a.id AS agent,
    ifnull(l.latest, r.session_id) AS session_id, NULL AS agent_id, r.waiting_for,
    r.waiting_since_ms AS since_ms
FROM registry AS r
LEFT JOIN latest_links AS l ON l.session_id = r.session_id
LEFT JOIN agents AS a ON a.session_id = ifnull(l.latest, r.session_id) AND a.agent_id IS NULL
WHERE r.gone = 0 AND (r.status IS 'waiting' AND r.waiting_for IS NOT NULL
    OR r.state IS 'blocked')
UNION ALL
SELECT 'hook', NULL, a.id, a.session_id, a.agent_id, a.hook_waiting_for, a.hook_at_ms
FROM agents AS a
WHERE a.hook_state = 'needs_you'
    AND a.session_id = (SELECT l.latest FROM latest_links AS l WHERE l.session_id = a.session_id)
    AND (EXISTS (SELECT 1 FROM registry AS r JOIN latest_links AS l ON l.session_id = r.session_id
            WHERE l.latest = a.session_id AND r.gone = 0)
        OR NOT EXISTS (SELECT 1 FROM registry AS r
            JOIN latest_links AS l ON l.session_id = r.session_id
            WHERE l.latest = a.session_id));
