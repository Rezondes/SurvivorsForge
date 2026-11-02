-- General addon discussion, separate from issue threads.
CREATE TABLE addon_comments (
 id BIGSERIAL PRIMARY KEY,
 addon_id BIGINT NOT NULL REFERENCES addons(id) ON DELETE CASCADE,
 author_id BIGINT NOT NULL REFERENCES users(id),
 body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 10000),
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 edited_at TIMESTAMPTZ,
 deleted_at TIMESTAMPTZ,
 deleted_by BIGINT REFERENCES users(id)
);
CREATE INDEX addon_comments_feed ON addon_comments(addon_id, created_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX addon_comments_author_recent ON addon_comments(author_id, created_at DESC);
