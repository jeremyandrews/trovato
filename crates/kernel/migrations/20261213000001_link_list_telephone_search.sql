-- Index a Link field's title and a List field's label, alongside the text
-- fields the vector already covers (B302).
--
-- A Link and a List field held no text of their own as far as search was
-- concerned: `item_search_vector` only ever read `Text`/`TextLong` field
-- names out of `item_type.settings`, so an item findable only by a link's
-- title or a chosen list value was invisible to search. A Link's `uri` and a
-- Telephone's number are never indexed: neither is prose a search term
-- matches.
--
-- A Link contributes its `title` (`p_fields->name->>'title'`), never its
-- `uri`. A List contributes the label of the key it stores, looked up in the
-- field's own `settings.allowed_values` (an array of `{"key", "label"}`): the
-- key alone (`"draft"`, `"gold"`) is rarely a search term a reader would
-- type, the label is. Both at the field's configured weight, `D` when it has
-- none, exactly like a text field.
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
        WITH field_kinds AS (
            -- Every field's real kind, read once so a name reaching `names`
            -- through `search_field_config` (an admin can give any field name
            -- a search weight there, not only a text one) is judged by what
            -- it actually is, not assumed to be text: a Telephone field given
            -- a weight that way must still never be indexed.
            SELECT def->>'field_name' AS field_name,
                   CASE
                       WHEN def->'field_type' = '"Link"'::jsonb THEN 'link'
                       WHEN def->'field_type' = '"List"'::jsonb THEN 'list'
                       WHEN def->'field_type' = '"Telephone"'::jsonb THEN 'telephone'
                       WHEN def->'field_type' = '"TextLong"'::jsonb THEN 'text'
                       WHEN jsonb_typeof(def->'field_type') = 'object'
                            AND def->'field_type' ? 'Text' THEN 'text'
                       ELSE NULL
                   END AS kind,
                   def->'settings'->'allowed_values' AS allowed_values
            FROM item_type it,
                 jsonb_array_elements(
                     CASE WHEN jsonb_typeof(it.settings->'fields') = 'array'
                          THEN it.settings->'fields' ELSE '[]'::jsonb END
                 ) AS def
            WHERE it.type = p_type
        ),
        declared AS (
            SELECT field_name, kind, allowed_values FROM field_kinds
            WHERE kind IN ('text', 'link', 'list')
        ),
        names AS (
            SELECT field_name, kind, allowed_values FROM declared
            UNION
            SELECT c.field_name, COALESCE(fk.kind, 'text') AS kind, fk.allowed_values
            FROM search_field_config c
            LEFT JOIN field_kinds fk ON fk.field_name = c.field_name
            WHERE c.bundle = p_type
              AND COALESCE(fk.kind, 'text') <> 'telephone'
            UNION
            SELECT unnest(ARRAY['field_body', 'body']), 'text', NULL::jsonb
        )
        SELECT n.field_name, COALESCE(c.weight, 'D') AS weight, n.kind, n.allowed_values
        FROM names n
        LEFT JOIN search_field_config c
            ON c.bundle = p_type AND c.field_name = n.field_name
        WHERE n.field_name IS NOT NULL
    LOOP
        IF field.kind = 'link' THEN
            IF jsonb_typeof(p_fields->field.field_name) = 'array' THEN
                SELECT string_agg(elem->>'title', ' ') INTO field_value
                FROM jsonb_array_elements(p_fields->field.field_name) AS elem;
            ELSE
                field_value := p_fields->field.field_name->>'title';
            END IF;
        ELSIF field.kind = 'list' THEN
            IF jsonb_typeof(p_fields->field.field_name) = 'array' THEN
                SELECT string_agg(av->>'label', ' ') INTO field_value
                FROM jsonb_array_elements(p_fields->field.field_name) AS k
                JOIN jsonb_array_elements(COALESCE(field.allowed_values, '[]'::jsonb)) AS av
                    ON av->>'key' = (k #>> '{}');
            ELSE
                SELECT av->>'label' INTO field_value
                FROM jsonb_array_elements(COALESCE(field.allowed_values, '[]'::jsonb)) AS av
                WHERE av->>'key' = (p_fields->field.field_name #>> '{}');
            END IF;
        ELSE
            field_value := COALESCE(
                p_fields->field.field_name->>'value',
                CASE WHEN jsonb_typeof(p_fields->field.field_name) = 'string'
                     THEN p_fields->>field.field_name END
            );
            IF p_fields->field.field_name->>'summary' IS NOT NULL THEN
                field_value := COALESCE(field_value, '') || ' '
                    || (p_fields->field.field_name->>'summary');
            END IF;
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

-- No backfill: no stored field had a Link or List type before this release,
-- so no existing row's vector changes by adding them.
