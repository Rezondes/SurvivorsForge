-- GitHub requests per hour, by what caused them (an API route or a worker job), the endpoint, the
-- quota they used (`read_token`, `anonymous`, `installation:<id>`, `app`, `oauth`, `download`) and
-- GitHub's status. Kept 90 days like api_request_stats.
CREATE TABLE github_request_stats (
    hour      TIMESTAMPTZ NOT NULL,
    origin    TEXT        NOT NULL,
    endpoint  TEXT        NOT NULL,
    quota     TEXT        NOT NULL,
    status    SMALLINT    NOT NULL,
    count     BIGINT      NOT NULL,
    PRIMARY KEY (hour, origin, endpoint, quota, status)
);

-- The last state of each quota as GitHub's x-ratelimit headers told it.
CREATE TABLE github_quota (
    quota      TEXT        NOT NULL,
    resource   TEXT        NOT NULL,
    "limit"    BIGINT      NOT NULL,
    used       BIGINT      NOT NULL,
    remaining  BIGINT      NOT NULL,
    reset_at   TIMESTAMPTZ,
    seen_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (quota, resource)
);
