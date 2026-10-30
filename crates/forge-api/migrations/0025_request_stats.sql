-- API requests per hour, route template, method, status and client (launcher, website, other).
-- Written by the API once a minute; buckets older than 90 days are deleted by a daily job.
CREATE TABLE api_request_stats (
    hour      TIMESTAMPTZ NOT NULL,
    route     TEXT        NOT NULL,
    method    TEXT        NOT NULL,
    status    SMALLINT    NOT NULL,
    client    TEXT        NOT NULL CHECK (client IN ('launcher', 'website', 'other')),
    count     BIGINT      NOT NULL,
    total_ms  BIGINT      NOT NULL,
    max_ms    BIGINT      NOT NULL,
    PRIMARY KEY (hour, route, method, status, client)
);
