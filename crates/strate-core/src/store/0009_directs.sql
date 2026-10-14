-- v9: director-launched units (#53). Store::open re-reads every event's fields
-- once after this runs (meta `rollup` 4), which also builds `directs`.

-- A cross-session message, joined on its msg_id alone (a SendMessage `to` or
-- an `origin.name` can carry a " [ref]" suffix or a whole `claude -n` line).
-- Sent: the msg_id of the SendMessage result a user record carries
-- (`toolUseResult.msg_id`), in a session or one of its subagents.
ALTER TABLE events ADD COLUMN sent_msg_id TEXT;
-- Received: on a user record from another session (`origin.kind` peer), its
-- `origin.msg_id` and the sender's `origin.name`.
ALTER TABLE events ADD COLUMN peer_msg_id TEXT;
ALTER TABLE events ADD COLUMN peer_name TEXT;
CREATE INDEX events_sent_msg ON events (sent_msg_id) WHERE sent_msg_id IS NOT NULL;
CREATE INDEX events_peer_msg ON events (peer_msg_id) WHERE peer_msg_id IS NOT NULL;

-- Which director session launched a unit session: the first `director-`
-- named session to message it, a cycle refused. Derived from the events
-- above and rebuilt whole, so it holds no state of its own.
CREATE TABLE directs (
    child TEXT PRIMARY KEY REFERENCES sessions (session_id),
    parent TEXT NOT NULL REFERENCES sessions (session_id),
    -- The message that made the edge.
    msg_id TEXT NOT NULL,
    CHECK (parent <> child)
) STRICT;
CREATE INDEX directs_parent ON directs (parent);
