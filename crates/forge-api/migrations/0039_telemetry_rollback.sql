-- A rollback is an install of a version older than the platform's newest, chosen by hand.
ALTER TABLE telemetry_events
    DROP CONSTRAINT telemetry_events_kind_check,
    ADD CONSTRAINT telemetry_events_kind_check CHECK (kind IN ('install', 'update', 'install_failed', 'update_failed', 'rollback'));
