-- Anonymous events of the launcher: installs and updates from the registry, and failed ones with
-- a fixed error code. No IP and no user agent are stored. `install_id` is the random ID a launcher
-- sends when the user allows it; it is set to NULL after 90 days, the counts stay.
CREATE TABLE telemetry_events (
    id                BIGSERIAL   PRIMARY KEY,
    day               DATE        NOT NULL,
    kind              TEXT        NOT NULL CHECK (kind IN ('install', 'update', 'install_failed', 'update_failed')),
    addon_id          BIGINT      NOT NULL REFERENCES addons (id) ON DELETE CASCADE,
    version           TEXT        NOT NULL,
    launcher_version  TEXT        NOT NULL,
    os                TEXT        NOT NULL CHECK (os IN ('windows', 'linux')),
    error_code        TEXT,
    install_id        UUID,
    created_at        TIMESTAMPTZ NOT NULL
);

CREATE INDEX telemetry_events_day ON telemetry_events (day);
CREATE INDEX telemetry_events_addon ON telemetry_events (addon_id, day);

-- One event per sender, addon, version, kind and day. The sender is the install ID or a keyed hash
-- of the IP whose key changes daily and lives only in the API's memory. Rows go after two days.
CREATE TABLE telemetry_seen (
    day       DATE   NOT NULL,
    sender    BYTEA  NOT NULL,
    addon_id  BIGINT NOT NULL,
    version   TEXT   NOT NULL,
    kind      TEXT   NOT NULL,
    PRIMARY KEY (day, sender, addon_id, version, kind)
);
