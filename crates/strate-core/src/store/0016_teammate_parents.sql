-- v16: a teammate whose team lead never resolved (#81). A team config
-- that is gone, or names a lead session with no transcript, left its
-- teammates' edges with no parent, and the live graph drew them with no
-- edge. Ingest now hangs such a teammate under the orchestrator of the
-- session whose subagents/ its transcript is in; this applies that rule
-- to the stored edges, a transcript since deleted included. A lead that
-- resolves on a later scan still replaces it.
UPDATE edges SET parent = (
    SELECT o.id FROM agents AS c
    JOIN agents AS o ON o.session_id = c.session_id AND o.agent_id IS NULL
    WHERE c.id = edges.child)
WHERE kind = 'teammate' AND parent IS NULL;
