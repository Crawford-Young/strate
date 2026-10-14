-- v10: the Audit's reads of events (#46). An events row carries its whole
-- record (`raw`), and the columns added after it sit past it on disk, so
-- reading a session's rows reads their records. These two indexes answer
-- the Audit's questions without the rows.

-- A session's last activity (its latest record that is not bookkeeping),
-- from the index alone.
CREATE INDEX events_session_time ON events (session_id, timestamp, type)
    WHERE timestamp IS NOT NULL;

-- The async launch results (#55): which dispatches ran in the background.
-- A query uses it only with this exact predicate.
CREATE INDEX events_async_launch ON events (session_id, tool_results)
    WHERE json_extract(raw, '$.toolUseResult.status') = 'async_launched';
