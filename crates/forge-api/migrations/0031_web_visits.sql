-- Page views and visitors of the website per day, counted by the website itself (no cookie, no
-- JavaScript). `kind` is the dimension: `site` (value ''), `page` (the route template, `target` the
-- addon slug or pack id), `referrer` (a domain or '(direct)'), `country` (Cloudflare's code) and
-- `device` (desktop, mobile, steam). Kept for good: they are sums without any person in them.
CREATE TABLE web_counts (
    day       DATE   NOT NULL,
    kind      TEXT   NOT NULL CHECK (kind IN ('site', 'page', 'referrer', 'country', 'device')),
    value     TEXT   NOT NULL,
    target    TEXT   NOT NULL DEFAULT '',
    views     BIGINT NOT NULL DEFAULT 0,
    visitors  BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (day, kind, value, target)
);

-- Which visitor was counted for what today. The sender is the website's keyed hash of the IP,
-- whose key changes daily and never leaves the website. Rows go after two days.
CREATE TABLE web_seen (
    day     DATE  NOT NULL,
    sender  BYTEA NOT NULL,
    kind    TEXT  NOT NULL,
    value   TEXT  NOT NULL,
    target  TEXT  NOT NULL,
    PRIMARY KEY (day, sender, kind, value, target)
);
