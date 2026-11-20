-- Index the files File fields reference, and promote every referenced file.
--
-- A File field stores the bare `file_managed.id` uuid (a string, or an array of
-- them when the field's cardinality is not 1). The reference index built by
-- 20260402000002 matched only storage paths embedded in field text, so a file
-- attached through a File field had no `file_reference` row: `/files/{path}`
-- treated it as an orphan (served to its uploader and admins only), and only
-- the admin content form promoted it out of Temporary, so files attached on any
-- other write path were deleted by the six hour temporary file cleanup.
--
-- The kernel now maintains these rows on every item write. This backfills them
-- for items already stored, then promotes every Temporary file that has a
-- reference.
--
-- Additive and safe to rerun: the INSERT skips existing rows, the UPDATE only
-- touches Temporary files. Reversing it means deleting the backfilled rows; no
-- data is lost either way. See UPGRADING.md.

-- Values are compared as text so a malformed value in a File field is skipped
-- rather than failing the cast.
INSERT INTO file_reference (file_id, item_id)
SELECT DISTINCT f.id, i.id
FROM item i
JOIN item_type t ON t.type = i.type
CROSS JOIN LATERAL jsonb_array_elements(
    CASE WHEN jsonb_typeof(t.settings -> 'fields') = 'array'
         THEN t.settings -> 'fields' ELSE '[]'::jsonb END
) AS d(def)
CROSS JOIN LATERAL jsonb_array_elements_text(
    CASE jsonb_typeof(i.fields -> (d.def ->> 'field_name'))
        WHEN 'array' THEN i.fields -> (d.def ->> 'field_name')
        WHEN 'string' THEN jsonb_build_array(i.fields -> (d.def ->> 'field_name'))
        ELSE '[]'::jsonb
    END
) AS v(value)
JOIN file_managed f ON f.id::text = lower(btrim(v.value))
WHERE d.def -> 'field_type' = '"File"'::jsonb
ON CONFLICT DO NOTHING;

-- 1 = Permanent, 0 = Temporary (`FileStatus` in crates/kernel/src/file/service.rs).
UPDATE file_managed
SET status = 1, changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE status = 0
  AND id IN (SELECT file_id FROM file_reference);
