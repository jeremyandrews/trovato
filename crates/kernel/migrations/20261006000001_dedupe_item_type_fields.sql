-- Drop duplicate field definitions from item_type.settings->fields.
--
-- `ContentTypeRegistry::add_field` pushed onto the list without looking at what
-- was already in it, and neither admin route checked either, so a type could
-- accumulate two or three definitions of the same `field_name`. Observed on
-- 2026-10-05: a test that added `search_test_field` to `page` on every run
-- reached three copies, and at that point the content translation form rendered
-- none of the type's fields at all. The registry now refuses the duplicate;
-- this repairs the rows already written, including types whose plugin is
-- disabled or uninstalled and so will never be re-registered.
--
-- The first definition of each name is the one kept, because it is the one the
-- type has been running with: a later copy only ever shadowed it.
--
-- No item data is touched. `item.fields` and `item_revision.fields` are JSONB
-- *objects* keyed by field name (`20260212000005_create_items.sql`), so one key
-- holds one value no matter how many times the type declared the field, and
-- removing a redundant definition cannot orphan a value.
--
-- Idempotent: rerunning finds no row whose list shrinks, so it writes nothing.
WITH exploded AS (
    SELECT t.type,
           f.value,
           f.ordinality
    FROM item_type t
    CROSS JOIN LATERAL
        jsonb_array_elements(t.settings -> 'fields') WITH ORDINALITY AS f(value, ordinality)
    WHERE jsonb_typeof(t.settings -> 'fields') = 'array'
),
kept AS (
    -- The first entry of each name...
    SELECT type, value, ordinality
    FROM (
        SELECT type,
               value,
               ordinality,
               row_number() OVER (
                   PARTITION BY type, value ->> 'field_name'
                   ORDER BY ordinality
               ) AS seq
        FROM exploded
        WHERE jsonb_exists(value, 'field_name')
    ) ranked
    WHERE seq = 1

    UNION ALL

    -- ...plus every entry carrying no `field_name` at all. Such an entry cannot
    -- be matched against another by name, so it is never a duplicate and is
    -- carried through untouched rather than collapsed with the others.
    SELECT type, value, ordinality
    FROM exploded
    WHERE NOT jsonb_exists(value, 'field_name')
),
deduped AS (
    SELECT type, jsonb_agg(value ORDER BY ordinality) AS fields
    FROM kept
    GROUP BY type
)
UPDATE item_type t
SET settings = jsonb_set(t.settings, '{fields}', d.fields)
FROM deduped d
WHERE t.type = d.type
  -- Only rows that actually hold a duplicate, so this neither rewrites every
  -- type nor does anything at all on a second run.
  AND jsonb_array_length(t.settings -> 'fields') <> jsonb_array_length(d.fields);
