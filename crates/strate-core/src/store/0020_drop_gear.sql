-- v20: gear goes (#106). Strate v2 is visibility and audit only: context
-- thresholds, caps and routing live with the agents, so nothing writes or
-- reads gear any more and the gauge colours against fixed 80 / 95. Dropping
-- the table deletes every stored gear row by design. The triggers and the
-- view read the table, so they go first.
DROP TRIGGER gear_order_on_insert;
DROP TRIGGER gear_order_on_update;
DROP VIEW gear_effective;
DROP TABLE gear_settings;
