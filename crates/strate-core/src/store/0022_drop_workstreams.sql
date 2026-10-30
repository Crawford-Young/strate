-- v22: workstreams go (#107). Work is grouped by each agent's charge (v21)
-- and the live tabs by `/clear` chain, so nothing reads the `/rename`
-- segments, the workstreams they named or `directs` (now the `message`
-- spawn edges) any more. Dropping them deletes their rows by design.

-- The views read the tables and columns below, so they go first.
DROP VIEW workstream_costs;
DROP VIEW event_workstreams;
DROP VIEW workstream_roots;

-- A column can be dropped once no index names it.
DROP INDEX audit_cells_segment;
ALTER TABLE audit_cells DROP COLUMN segment_id;
ALTER TABLE agents DROP COLUMN segment_id;
ALTER TABLE sessions DROP COLUMN workstream_id;

DROP TABLE directs;
-- Segments reference workstreams, so they go before them.
DROP TABLE segments;
DROP TABLE workstreams;

-- $ per effective charge (`agent_charges`; charge NULL is Unlinked): each
-- request counts toward its agent's charge. A request whose agent has no
-- row is Unlinked.
CREATE VIEW charge_costs AS
SELECT x.charge, sum(r.cost_usd) AS cost_usd, count(*) - count(r.cost_usd) AS unpriced
FROM requests AS r
LEFT JOIN agents AS a ON a.session_id = r.session_id
    AND ifnull(a.agent_id, '') = ifnull(r.agent_id, '')
LEFT JOIN agent_charges AS x ON x.agent = a.id
GROUP BY x.charge;
