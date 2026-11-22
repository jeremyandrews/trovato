-- Per content type search switches (B106 part 2).
--
-- Two settings under `item_type.settings.search`, both on unless set to
-- false: `include` (the type is searched at all) and `comments` (its items'
-- comments are searched with them). A site that never sets them indexes
-- everything, as before this migration. The Pagefind export reads the same two
-- keys; this teaches the vector function to honour them. The admin screen that
-- changes them recomputes the type's vectors itself, so nothing is backfilled
-- here: no row has either key yet.

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
        SELECT id, body FROM comment
        WHERE item_id = p_id AND parent_id IS NULL AND status = 1
        UNION ALL
        SELECT c.id, c.body FROM comment c
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
