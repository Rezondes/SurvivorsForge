-- An addon of a pack either stays on its version or follows the newest checked one; the version
-- of a following addon moves up whenever the pack is shown or downloaded.
ALTER TABLE pack_versions ADD COLUMN latest BOOLEAN NOT NULL DEFAULT false;
