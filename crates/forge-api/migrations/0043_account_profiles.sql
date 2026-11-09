-- The launcher's profiles (named sets of addons, each on or off) per account, so another computer
-- of the same user gets them too. `data` is the launcher's profile as it sends it; `updated_at`
-- is when the launcher changed it (seconds since 1970), so the newer side wins a sync.
CREATE TABLE account_profiles (
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT   NOT NULL,
    data       JSONB  NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX account_profiles_name ON account_profiles (user_id, lower(name));
