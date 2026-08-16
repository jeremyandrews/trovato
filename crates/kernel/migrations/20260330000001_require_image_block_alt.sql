-- Backfill `alt` field on existing image blocks that lack it.
-- Sets `alt` to empty string (decorative image per WCAG) so the field exists
-- in the JSONB for all image blocks. The kernel's block validation now
-- requires the `alt` key to be present (empty string is valid).

-- Update item.fields: find image blocks missing `alt` and add `alt: ""`
UPDATE item
SET fields = (
    SELECT jsonb_object_agg(field_key, field_value)
    FROM (
        SELECT field_key,
            CASE
                WHEN field_value @> '[]'::jsonb THEN (
                    SELECT jsonb_agg(
                        CASE
                            WHEN block->>'type' = 'image'
                                AND block->'data' IS NOT NULL
                                AND NOT (block->'data' ? 'alt')
                            THEN jsonb_set(block, '{data,alt}', '""'::jsonb)
                            ELSE block
                        END
                    )
                    FROM jsonb_array_elements(field_value) AS block
                )
                ELSE field_value
            END AS field_value
        FROM jsonb_each(item.fields) AS kv(field_key, field_value)
    ) AS patched
)
WHERE fields::text LIKE '%"type":"image"%'
  AND fields::text NOT LIKE '%"alt"%';

-- Same for item_revision.fields
UPDATE item_revision
SET fields = (
    SELECT jsonb_object_agg(field_key, field_value)
    FROM (
        SELECT field_key,
            CASE
                WHEN field_value @> '[]'::jsonb THEN (
                    SELECT jsonb_agg(
                        CASE
                            WHEN block->>'type' = 'image'
                                AND block->'data' IS NOT NULL
                                AND NOT (block->'data' ? 'alt')
                            THEN jsonb_set(block, '{data,alt}', '""'::jsonb)
                            ELSE block
                        END
                    )
                    FROM jsonb_array_elements(field_value) AS block
                )
                ELSE field_value
            END AS field_value
        FROM jsonb_each(item_revision.fields) AS kv(field_key, field_value)
    ) AS patched
)
WHERE fields::text LIKE '%"type":"image"%'
  AND fields::text NOT LIKE '%"alt"%';
