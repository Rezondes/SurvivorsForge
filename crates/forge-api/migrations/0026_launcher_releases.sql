-- Download counts of the launcher's own release files on GitHub (LAUNCHER_REPO), refreshed hourly
-- by the job `poll_launcher_releases`. Self-updates count as downloads too.
CREATE TABLE launcher_releases (
    tag             TEXT        NOT NULL,
    asset           TEXT        NOT NULL,
    published_at    TIMESTAMPTZ,
    prerelease      BOOLEAN     NOT NULL,
    download_count  BIGINT      NOT NULL,
    fetched_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tag, asset)
);
