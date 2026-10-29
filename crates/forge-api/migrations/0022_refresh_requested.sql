-- When the user last forced an update check of an addon; one per user every five minutes.
ALTER TABLE users ADD COLUMN refresh_requested_at TIMESTAMPTZ;
