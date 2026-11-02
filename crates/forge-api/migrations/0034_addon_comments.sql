-- Comments under an addon: a public discussion, apart from the GitHub issues of its repo.
-- Deleting only marks a comment, so replies keep their place and moderators can look back.
CREATE TABLE addon_comments (
    id         BIGSERIAL   PRIMARY KEY,
    addon_id   BIGINT      NOT NULL REFERENCES addons (id) ON DELETE CASCADE,
    author_id  BIGINT      NOT NULL REFERENCES users (id),
    body       TEXT        NOT NULL CHECK (char_length(body) BETWEEN 1 AND 5000),
    parent_id  BIGINT      REFERENCES addon_comments (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL,
    edited_at  TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    deleted_by BIGINT      REFERENCES users (id)
);

CREATE INDEX addon_comments_feed ON addon_comments (addon_id, id DESC);
CREATE INDEX addon_comments_author_recent ON addon_comments (author_id, created_at DESC);

-- Both tabs start off; the owner turns them on. Comments turned off again stay and show again
-- when they are back on. The Issues tab shows the repo's GitHub issues only while it is on.
ALTER TABLE addons ADD COLUMN comments_enabled BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE addons ADD COLUMN issues_enabled BOOLEAN NOT NULL DEFAULT false;

-- Comments are reported through the same queue as addons, versions and images.
ALTER TABLE reports DROP CONSTRAINT reports_target_type_check;
ALTER TABLE reports ADD CONSTRAINT reports_target_type_check CHECK (target_type IN ('addon', 'version', 'image', 'comment'));
