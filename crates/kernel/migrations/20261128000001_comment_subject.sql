-- A comment's optional plain text subject (B163).
--
-- The table had no column for one, so a site importing comments with titles
-- (Drupal comments have a subject field) had nowhere to put the title but
-- inside the body, which changed the body and could not be edited
-- separately. Nullable, no default: an imported comment with no title stays
-- exactly as it was, and no row is backfilled.

-- The length check travels with the column's own definition rather than as a
-- separately named constraint added afterward: every row that exists before
-- this runs gets NULL in the new column, which satisfies it trivially, and so
-- does every insert the previous release's code still makes (it does not know
-- the column exists, so it never supplies one). Matches the Rust-side limit
-- (`models::comment::SUBJECT_MAX_CHARS`), which turns a submission over the
-- limit into a 400 before it ever reaches this constraint; the constraint is
-- the backstop for anything that writes to the column some other way (an
-- import, a plugin).
ALTER TABLE comment ADD COLUMN subject TEXT
    CHECK (subject IS NULL OR char_length(subject) <= 255);

-- The vector function now folds the subject in alongside the body, at the
-- body's own weight: a subject is not ranked differently from the text it
-- labels, and a comment with no subject contributes exactly what it always
-- did. The trigger is extended to fire on `subject` too, so an edited
-- subject keeps the vector current the same way an edited body already does.
CREATE OR REPLACE FUNCTION item_search_vector(
    p_id UUID,
    p_type TEXT,
    p_title TEXT,
    p_fields JSONB
) RETURNS tsvector AS $$
DECLARE
    field RECORD;
    vector tsvector;
    field_value TEXT;
    comment_text TEXT;
    switches JSONB;
BEGIN
    SELECT settings->'search' INTO switches FROM item_type WHERE type = p_type;
    IF jsonb_typeof(switches) = 'object' AND switches->'include' = 'false'::jsonb THEN
        RETURN ''::tsvector;
    END IF;

    vector := setweight(to_tsvector('english', COALESCE(p_title, '')), 'A');

    FOR field IN
        WITH declared AS (
            SELECT def->>'field_name' AS field_name
            FROM item_type it,
                 jsonb_array_elements(
                     CASE WHEN jsonb_typeof(it.settings->'fields') = 'array'
                          THEN it.settings->'fields' ELSE '[]'::jsonb END
                 ) AS def
            WHERE it.type = p_type
              AND (def->'field_type' = '"TextLong"'::jsonb
                   OR (jsonb_typeof(def->'field_type') = 'object'
                       AND def->'field_type' ? 'Text'))
        ),
        names AS (
            SELECT field_name FROM declared
            UNION
            SELECT field_name FROM search_field_config WHERE bundle = p_type
            UNION
            SELECT unnest(ARRAY['field_body', 'body'])
        )
        SELECT n.field_name, COALESCE(c.weight, 'D') AS weight
        FROM names n
        LEFT JOIN search_field_config c
            ON c.bundle = p_type AND c.field_name = n.field_name
        WHERE n.field_name IS NOT NULL
    LOOP
        field_value := COALESCE(
            p_fields->field.field_name->>'value',
            CASE WHEN jsonb_typeof(p_fields->field.field_name) = 'string'
                 THEN p_fields->>field.field_name END
        );
        IF p_fields->field.field_name->>'summary' IS NOT NULL THEN
            field_value := COALESCE(field_value, '') || ' '
                || (p_fields->field.field_name->>'summary');
        END IF;

        IF field_value IS NOT NULL AND field_value <> '' THEN
            vector := vector || setweight(
                to_tsvector('english', regexp_replace(field_value, '<[^>]*>', ' ', 'g')),
                field.weight::"char"
            );
        END IF;
    END LOOP;

    IF jsonb_typeof(switches) = 'object' AND switches->'comments' = 'false'::jsonb THEN
        RETURN vector;
    END IF;

    WITH RECURSIVE visible AS (
        SELECT id, COALESCE(subject || ' ', '') || body AS body FROM comment
        WHERE item_id = p_id AND parent_id IS NULL AND status = 1
        UNION ALL
        SELECT c.id, COALESCE(c.subject || ' ', '') || c.body FROM comment c
        JOIN visible v ON c.parent_id = v.id
        WHERE c.status = 1
    )
    SELECT string_agg(body, ' ') INTO comment_text FROM visible;

    IF comment_text IS NOT NULL AND comment_text <> '' THEN
        vector := vector || setweight(
            to_tsvector('english', regexp_replace(comment_text, '<[^>]*>', ' ', 'g')),
            'D'
        );
    END IF;

    RETURN vector;
END;
$$ LANGUAGE plpgsql STABLE;

-- Replaces the trigger in place rather than DROP then CREATE: the previous
-- release's writes still go through a trigger named `trg_comment_search` on
-- `comment` for the whole span of this statement, never a window with none.
CREATE OR REPLACE TRIGGER trg_comment_search
    AFTER INSERT OR DELETE OR UPDATE OF subject, body, status, parent_id, item_id ON comment
    FOR EACH ROW
    EXECUTE FUNCTION comment_search_update();

-- No backfill: every existing comment's subject is NULL, so
-- `COALESCE(subject || ' ', '') || body` equals `body` for every row that
-- exists today — the trigger now watches a column, but no stored vector
-- actually changes, so nothing here is recomputed.
