-- v7: the #26 review. Why GitHub closed an issue (`stateReason`:
-- COMPLETED, NOT_PLANNED, DUPLICATE; NULL while open, and on rows cached
-- before v7 until their next refresh). Only COMPLETED closures count in the
-- Audit headline.
ALTER TABLE gh_issues ADD COLUMN state_reason TEXT;

-- The Audit resolves each window of the cells it reads by segment.
CREATE INDEX audit_cells_segment ON audit_cells (segment_id);
