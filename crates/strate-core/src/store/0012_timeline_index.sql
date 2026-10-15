-- v12: the Audit timeline's replay (#61). As in v10, the timing columns
-- sit past `raw` on disk, so replaying a session from its rows reads every
-- record. This index holds what the replay reads (account.rs `LINES_SQL`),
-- each agent's records in transcript order.
CREATE INDEX events_timeline ON events (
    session_id, agent_id, byte_offset, timestamp, type, request_key, tool_uses, tool_results,
    prompt
) WHERE timestamp IS NOT NULL;
