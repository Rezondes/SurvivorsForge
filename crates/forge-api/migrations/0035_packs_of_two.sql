-- A pack is for several addons: a single one downloads on its own from its page. Packs of one
-- addon go; their zips leave with the retention of `pack_files` like any other unused zip.
DELETE FROM packs WHERE id IN (SELECT pack_id FROM pack_versions GROUP BY pack_id HAVING count(*) = 1);

-- New packs show on the public pack list unless their author hides them.
ALTER TABLE packs ALTER COLUMN listed SET DEFAULT true;
