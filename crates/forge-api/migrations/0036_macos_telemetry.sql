ALTER TABLE telemetry_events
    DROP CONSTRAINT telemetry_events_os_check,
    ADD CONSTRAINT telemetry_events_os_check CHECK (os IN ('windows', 'linux', 'macos'));
