-- v24: intents go (#122). v1 stored launch and dispatch requests here, but
-- strate v2 only reads (#105): nothing launches or dispatches, and no code
-- reads or writes the table. No index, trigger or view names it, so the
-- table goes alone. Dropping it deletes any stored intent by design.
DROP TABLE intents;
