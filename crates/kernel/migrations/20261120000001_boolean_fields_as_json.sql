-- Store every Boolean field value as a JSON boolean.
--
-- Both item forms posted a checked Boolean as `value="1"` and stored the raw
-- string, so an item saved from a form held `"1"` where a plugin or a config
-- import held `true`. A gather filter compares `fields->>'name'` with `'true'`,
-- so a box ticked in the admin form never matched "Online only". The forms now
-- store JSON `true` and `false` (`content::submission::build_item_fields`);
-- this rewrites the strings already written, in items and in their revisions,
-- so a revert does not bring a string back.
--
-- Only fields a type declares as `Boolean`, and only values that are JSON
-- strings: `"1"`, `"true"` and `"on"` become `true`, anything else (`"0"`,
-- `""`) becomes `false`. A value that is already a boolean, a number or
-- absent is not touched.
--
-- One statement per field, because an `UPDATE … FROM` touches each row once
-- and an item can have several Boolean fields.
--
-- Idempotent: a second run finds no string value left to rewrite.
DO $$
DECLARE
    f record;
BEGIN
    FOR f IN
        SELECT t.type AS item_type, d.value ->> 'field_name' AS name
        FROM item_type t
        CROSS JOIN LATERAL jsonb_array_elements(t.settings -> 'fields') AS d(value)
        WHERE jsonb_typeof(t.settings -> 'fields') = 'array'
          AND d.value ->> 'field_type' = 'Boolean'
          AND d.value ->> 'field_name' IS NOT NULL
    LOOP
        UPDATE item
        SET fields = jsonb_set(fields, ARRAY[f.name],
                               to_jsonb(fields ->> f.name IN ('1', 'true', 'on')))
        WHERE type = f.item_type
          AND jsonb_typeof(fields -> f.name) = 'string';

        UPDATE item_revision r
        SET fields = jsonb_set(r.fields, ARRAY[f.name],
                               to_jsonb(r.fields ->> f.name IN ('1', 'true', 'on')))
        FROM item i
        WHERE r.item_id = i.id
          AND i.type = f.item_type
          AND jsonb_typeof(r.fields -> f.name) = 'string';
    END LOOP;
END
$$;
