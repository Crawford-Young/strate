-- v8: performance (#51). Store::open re-reads every event's fields and
-- recomputes every rollup and cell once after this runs (meta `rollup`).

-- Of a cell's waiting (wait_ms), the time spent on questions to the user
-- (AskUserQuestion, ExitPlanMode) rather than on the next typed prompt.
ALTER TABLE audit_cells ADD COLUMN ask_ms INTEGER NOT NULL DEFAULT 0;

-- A task notification (a queue-operation, a queued_command attachment or a
-- notification prompt; one notification repeats across them): its
-- <task-id> (an agent id, or a background shell's) and <status>.
ALTER TABLE events ADD COLUMN notification_task TEXT;
ALTER TABLE events ADD COLUMN notification_status TEXT;
-- A JSON array of the record's SendMessage `to` values.
ALTER TABLE events ADD COLUMN message_to TEXT;
-- On an isApiErrorMessage record: prompt_too_long | other.
ALTER TABLE events ADD COLUMN api_error TEXT;
CREATE INDEX events_notification ON events (session_id, notification_task)
    WHERE notification_task IS NOT NULL;

-- A dispatch's outcome: the worst status its notifications carry (failed,
-- then killed, then completed), and blocked when it completed with a
-- `NEEDS_CONTEXT:` line in its final report. NULL: never notified.
ALTER TABLE agents ADD COLUMN outcome TEXT
    CHECK (outcome IN ('completed', 'failed', 'killed', 'blocked'));
-- SendMessage calls in its session to its id or name; NULL on an
-- orchestrator.
ALTER TABLE agents ADD COLUMN followups INTEGER;
-- The deepest of its requests as a % of its model's context window.
ALTER TABLE agents ADD COLUMN context_peak_pct REAL;
