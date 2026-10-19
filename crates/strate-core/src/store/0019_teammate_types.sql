-- v19: a teammate's type (#86). Builds before it stored each teammate's
-- meta `agentType`, which is the teammate's name. Every discovery scan now
-- writes the type it was spawned as, but a teammate whose meta.json is gone
-- is never scanned again, so it reads `teammate` (no type recorded) rather
-- than its name. A scan after this rewrites every teammate whose meta remains.
UPDATE agents SET agent_type = 'teammate'
WHERE kind = 'teammate' AND agent_type IS name;
