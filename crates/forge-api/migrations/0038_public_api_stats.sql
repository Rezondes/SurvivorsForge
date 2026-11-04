-- Requests to the public API (docs/public-api.md) by other clients, per hour: the first word of their
-- User-Agent, which the API's rules ask for (`MyLauncher/1.2`), and the country Cloudflare reports.
-- No IP address. Pruned with the other request stats after 90 days.
CREATE TABLE public_api_stats (
    hour     TIMESTAMPTZ NOT NULL,
    route    TEXT        NOT NULL,
    client   TEXT        NOT NULL,
    country  TEXT        NOT NULL,
    count    BIGINT      NOT NULL,
    PRIMARY KEY (hour, route, client, country)
);
