-- Launchers that asked the API something, one row per installation and hour, with the version and
-- platform their User-Agent names. Grafana counts them per hour and per day. Only launchers that
-- send their anonymous installation ID count; the telemetry prune forgets rows with the other IDs.
CREATE TABLE launcher_active (
    hour        TIMESTAMPTZ NOT NULL,
    install_id  TEXT        NOT NULL,
    version     TEXT        NOT NULL,
    os          TEXT        NOT NULL,
    PRIMARY KEY (hour, install_id)
);
