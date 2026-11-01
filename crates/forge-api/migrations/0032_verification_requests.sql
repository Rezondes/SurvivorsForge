-- Creators of an addon that no admin has verified yet ask for the check by hand. They post the
-- addon in the Discord channel verified-addons and send the link; admins approve (which verifies
-- the version asked about) or reject with a reason.
CREATE TABLE verification_requests (
    id            BIGSERIAL   PRIMARY KEY,
    addon_id      BIGINT      NOT NULL REFERENCES addons (id) ON DELETE CASCADE,
    version_id    BIGINT      NOT NULL REFERENCES versions (id) ON DELETE CASCADE,
    requested_by  BIGINT      NOT NULL REFERENCES users (id),
    discord_url   TEXT        NOT NULL,
    status        TEXT        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'approved', 'rejected')),
    note          TEXT        NOT NULL DEFAULT '',
    decided_by    BIGINT      REFERENCES users (id),
    created_at    TIMESTAMPTZ NOT NULL,
    decided_at    TIMESTAMPTZ
);

-- One open request per addon.
CREATE UNIQUE INDEX verification_requests_open ON verification_requests (addon_id) WHERE status = 'open';
CREATE INDEX verification_requests_addon ON verification_requests (addon_id, created_at DESC);
