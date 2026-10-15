-- v14: the orchestrator's registry fields go (#68). The views read a
-- registry row as its process's latest /clear link, so the copy that live
-- polls kept on the session the row names, the chain's first link, went
-- stale; v13's build stopped writing it and nothing reads it.
ALTER TABLE agents DROP COLUMN registry_name;
ALTER TABLE agents DROP COLUMN registry_state;
