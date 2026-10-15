-- v17: a teammate's turn end (#83). Claude Code delivers it to the team
-- lead as a user record in the lead's own transcript, holding an
-- idle_notification teammate-message. The live graph reads the drawn
-- session's notices from the stored records on every read, so an
-- incremental build reads as a cold one; this index keeps that read to
-- the few records that can hold one.
CREATE INDEX events_idle_notices ON events (session_id)
    WHERE type = 'user' AND agent_id IS NULL AND instr(raw, 'idle_notification') > 0;
