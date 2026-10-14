-- v10: the Audit's reads of events (#46). An events row carries its whole
-- record (`raw`), and the columns added after it sit past it on disk, so
-- reading a session's rows reads their records. This index answers the
-- Audit's question without the rows.

-- A session's last activity (its latest record that is not bookkeeping),
-- from the index alone.
CREATE INDEX events_session_time ON events (session_id, timestamp, type)
    WHERE timestamp IS NOT NULL;
