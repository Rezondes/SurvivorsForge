-- Community issues are separate from security reports.
CREATE TABLE addon_issues (
    id BIGSERIAL PRIMARY KEY,
    addon_id BIGINT NOT NULL REFERENCES addons(id) ON DELETE CASCADE,
    author_id BIGINT NOT NULL REFERENCES users(id),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 3 AND 160),
    body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 10000),
    kind TEXT NOT NULL DEFAULT 'bug' CHECK (kind IN ('bug', 'feature', 'question')),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'in_progress', 'resolved', 'closed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX addon_issues_addon_status ON addon_issues(addon_id, status, created_at DESC);
CREATE INDEX addon_issues_author ON addon_issues(author_id, created_at DESC);

CREATE TABLE addon_issue_comments (
    id BIGSERIAL PRIMARY KEY,
    issue_id BIGINT NOT NULL REFERENCES addon_issues(id) ON DELETE CASCADE,
    author_id BIGINT NOT NULL REFERENCES users(id),
    body TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 10000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX addon_issue_comments_issue ON addon_issue_comments(issue_id, created_at, id);
