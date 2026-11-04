-- Numbers kept now for the addon and creator statistics pages to come.
--
-- GitHub's download count of each version per day: `versions` holds only the current total, so the
-- curve over time has to be written down as it goes (`snapshot_github_downloads`). The day's row
-- is the last count seen that day.
CREATE TABLE version_downloads (
    day         DATE   NOT NULL,
    version_id  BIGINT NOT NULL REFERENCES versions (id) ON DELETE CASCADE,
    downloads   BIGINT NOT NULL,
    PRIMARY KEY (day, version_id)
);

CREATE INDEX version_downloads_version ON version_downloads (version_id, day);

-- Where the visitors of an addon's pages come from and their country (`target` the slug), next to
-- the site-wide `referrer` and `country`.
ALTER TABLE web_counts
    DROP CONSTRAINT web_counts_kind_check,
    ADD CONSTRAINT web_counts_kind_check CHECK (kind IN ('site', 'page', 'referrer', 'country', 'device', 'search_miss', 'addon_referrer', 'addon_country'));
