-- Deduplicate conference items created by concurrent queue workers and add a
-- unique partial index to prevent future duplicates at the database level.
--
-- Root cause: tap_queue_worker runs with concurrency = 4. When multiple workers
-- process different topic files simultaneously, they each load the
-- source_id → item_id dedup map before any insert commits. All see the same
-- conference as "not found" and all insert it, producing 3–4 copies.
--
-- Fix: merge topics from all duplicates into the oldest record, delete the
-- extras, then create a unique index so future concurrent inserts hit
-- ON CONFLICT instead of silently succeeding.

-- Step 1: Merge field_topics from all duplicate copies into the canonical
-- (earliest-created, lowest-id on tie) record.
WITH canonical AS (
    SELECT DISTINCT ON (fields->>'field_source_id')
        id,
        fields->>'field_source_id' AS source_id
    FROM item
    WHERE type = 'conference'
      AND fields->>'field_source_id' IS NOT NULL
      AND fields->>'field_source_id' != ''
    ORDER BY fields->>'field_source_id', created ASC, id ASC
),
merged_topics AS (
    SELECT
        c.id AS canonical_id,
        (
            SELECT COALESCE(jsonb_agg(t ORDER BY t), '[]'::jsonb)
            FROM (
                SELECT jsonb_array_elements_text(i.fields->'field_topics')
                FROM item i
                WHERE i.type = 'conference'
                  AND i.fields->>'field_source_id' = c.source_id
                GROUP BY 1   -- UNION-style dedup via GROUP BY
            ) u(t)
        ) AS merged
    FROM canonical c
)
UPDATE item
SET fields = fields || jsonb_build_object('field_topics', mt.merged)
FROM merged_topics mt
WHERE item.id = mt.canonical_id;

-- Step 2: Delete all non-canonical duplicate conference items.
DELETE FROM item
WHERE id IN (
    SELECT id FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY fields->>'field_source_id'
                   ORDER BY created ASC, id ASC
               ) AS rn
        FROM item
        WHERE type = 'conference'
          AND fields->>'field_source_id' IS NOT NULL
          AND fields->>'field_source_id' != ''
    ) t
    WHERE rn > 1
);

-- Step 3: Add the unique index so concurrent inserts resolve via ON CONFLICT
-- rather than producing duplicates.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_item_conference_source_id
    ON item ((fields->>'field_source_id'))
    WHERE type = 'conference'
      AND fields->>'field_source_id' IS NOT NULL
      AND fields->>'field_source_id' != '';
