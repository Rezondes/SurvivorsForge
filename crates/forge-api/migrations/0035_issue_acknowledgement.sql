-- Preserve existing issue records while adding a maintainer acknowledgement state.
ALTER TABLE addon_issues DROP CONSTRAINT IF EXISTS addon_issues_status_check;
ALTER TABLE addon_issues ADD CONSTRAINT addon_issues_status_check
  CHECK (status IN ('open', 'acknowledged', 'in_progress', 'resolved', 'closed'));
