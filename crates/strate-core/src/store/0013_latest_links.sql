-- v13: needs-you on a /clear chain's latest link (#68). `claude agents`
-- keys a process by the session it started with, so after a /clear its
-- registry row still names the chain's first link while the latest one
-- runs, and an earlier link's unanswered prompt went with the /clear.

-- Each session's /clear chain's latest link, by start, then id: of the
-- session itself and the sessions sharing its process root or having its
-- id as theirs. A registry row describes the latest link of the session
-- it names.
CREATE VIEW latest_links AS
SELECT s.session_id, (SELECT l.session_id FROM sessions AS l
        WHERE l.session_id = s.session_id
            OR l.process_root = ifnull(s.process_root, s.session_id)
        ORDER BY l.started_at IS NULL, l.started_at DESC, l.session_id DESC LIMIT 1) AS latest
FROM sessions AS s;

-- As in v11, but on latest links: a registry wait is its row's latest
-- link's, and a background session `blocked` on the user (no pid, status
-- or waitingFor) waits too. A hook's needs-you is listed only on its
-- chain's latest link, while some registry row of that link is live, or
-- none describes it at all.
DROP VIEW needs_you;
CREATE VIEW needs_you AS
SELECT 'registry' AS source, r.id AS registry, a.id AS agent,
    ifnull(l.latest, r.session_id) AS session_id, NULL AS agent_id, r.waiting_for,
    r.last_seen_ms AS since_ms
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
