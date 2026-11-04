-- Searches of the addon list that found nothing (`value` the lowercased term), counted like page
-- views. They are kept 90 days (`prune_web_seen`), not for good: they are text a visitor typed.
ALTER TABLE web_counts
    DROP CONSTRAINT web_counts_kind_check,
    ADD CONSTRAINT web_counts_kind_check CHECK (kind IN ('site', 'page', 'referrer', 'country', 'device', 'search_miss'));
