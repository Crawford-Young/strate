-- v8: performance (#51). Of a cell's waiting (wait_ms), the time spent on
-- questions to the user (AskUserQuestion, ExitPlanMode) rather than on the
-- next typed prompt. Store::open recomputes every cell once after this runs
-- (meta `rollup`).
ALTER TABLE audit_cells ADD COLUMN ask_ms INTEGER NOT NULL DEFAULT 0;
