-- Threaded addon comments and private user reports. Additive only.
ALTER TABLE addon_comments
  ADD COLUMN parent_id BIGINT REFERENCES addon_comments(id) ON DELETE SET NULL,
  ADD COLUMN quote_id BIGINT REFERENCES addon_comments(id) ON DELETE SET NULL;
CREATE INDEX addon_comments_parent_idx ON addon_comments(parent_id, id) WHERE deleted_at IS NULL;
CREATE TABLE addon_comment_reports (
  id BIGSERIAL PRIMARY KEY,
  comment_id BIGINT NOT NULL REFERENCES addon_comments(id) ON DELETE CASCADE,
  reporter_id BIGINT NOT NULL REFERENCES users(id),
  reason TEXT NOT NULL CHECK (reason IN ('spam','harassment','hate','unsafe','other')),
  details TEXT NOT NULL DEFAULT '' CHECK (char_length(details) <= 2000),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  resolved_at TIMESTAMPTZ,
  resolved_by BIGINT REFERENCES users(id),
  resolution_note TEXT,
  UNIQUE (comment_id, reporter_id)
);
CREATE INDEX addon_comment_reports_pending_idx ON addon_comment_reports(created_at DESC) WHERE resolved_at IS NULL;
