-- The launcher in numbers, one row per `poll_launcher_releases` run, so Grafana can draw them over
-- time: launchers active in the last 30 days (as on /stats) and downloads over all releases
-- (`launcher_releases` holds only the latest). `downloads` stays empty when GitHub did not answer.
CREATE TABLE launcher_counts (
    at          TIMESTAMPTZ PRIMARY KEY,
    active_30d  BIGINT      NOT NULL,
    downloads   BIGINT
);
