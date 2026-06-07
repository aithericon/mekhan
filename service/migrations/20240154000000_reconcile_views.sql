-- Legacy file migration — Phase 4 reconcile views (docs/32 §4/§5).
--
-- Read-only classification of the crawl-observed `file_inventory` against the
-- legacy ArangoDB baseline `legacy_file_index`. Crawl is metadata-only
-- ({path,size,mtime}, NO hash) so reconcile inherits the legacy hash by
-- matching (file_server_id, path); size mismatch ⇒ corruption. `orphan_db`
-- (a legacy row never observed on disk) is a REPORT over staging, not an
-- inventory row.

-- Legacy rows with no observed physical copy on disk.
CREATE VIEW reconcile_orphan_db AS
    SELECT li.*
    FROM legacy_file_index li
    LEFT JOIN file_inventory fi
        ON fi.file_server_id = li.file_server_id AND fi.path = li.path
    WHERE fi.id IS NULL;

-- Same content observed on more than one physical copy.
CREATE VIEW reconcile_duplicates AS
    SELECT content_hash,
           count(*)                                                        AS copies,
           array_agg(file_server_id || ':' || path
                     ORDER BY file_server_id, path)                        AS locations,
           bool_or(is_canonical)                                           AS has_canonical
    FROM file_inventory
    WHERE content_hash IS NOT NULL
    GROUP BY content_hash
    HAVING count(*) > 1;

-- Inventory counts by status. The orphan_db count comes separately from the
-- reconcile_orphan_db view (staging-side, not an inventory status).
CREATE VIEW reconcile_summary AS
    SELECT status, count(*) AS n
    FROM file_inventory
    GROUP BY status;
