-- Downloads the platform sees itself, per day: `website` (an addon version over the website,
-- `target_id` = addon) and `pack` (a built pack, `target_id` = pack). Downloads straight from
-- GitHub stay unknown; launcher installs are in telemetry_events.
CREATE TABLE download_counts (
    day        DATE   NOT NULL,
    source     TEXT   NOT NULL CHECK (source IN ('website', 'pack')),
    target_id  BIGINT NOT NULL,
    count      BIGINT NOT NULL,
    PRIMARY KEY (day, source, target_id)
);

-- One count per visitor, target and day; the visitor is a keyed hash of the IP (see
-- telemetry_seen). Rows go after two days.
CREATE TABLE download_seen (
    day        DATE   NOT NULL,
    sender     BYTEA  NOT NULL,
    source     TEXT   NOT NULL,
    target_id  BIGINT NOT NULL,
    PRIMARY KEY (day, sender, source, target_id)
);
