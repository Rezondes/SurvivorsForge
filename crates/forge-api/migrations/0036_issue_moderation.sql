-- Issue lifecycle and reversible moderation, preserving existing conversations.
ALTER TABLE addon_issues ADD COLUMN acknowledged_at TIMESTAMPTZ;
ALTER TABLE addon_issues ADD COLUMN deleted_at TIMESTAMPTZ;
ALTER TABLE addon_issues ADD COLUMN edited_at TIMESTAMPTZ;
ALTER TABLE addon_issue_comments ADD COLUMN deleted_at TIMESTAMPTZ;
ALTER TABLE addon_issue_comments ADD COLUMN edited_at TIMESTAMPTZ;
CREATE TABLE addon_issue_events (
  id BIGSERIAL PRIMARY KEY,
  issue_id BIGINT NOT NULL REFERENCES addon_issues(id) ON DELETE CASCADE,
  actor_id BIGINT NOT NULL REFERENCES users(id),
  action TEXT NOT NULL CHECK (action IN ('status', 'issue_edited', 'issue_deleted', 'comment_edited', 'comment_deleted')),
  old_value TEXT,
  new_value TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX addon_issue_events_issue ON addon_issue_events(issue_id, created_at, id);
