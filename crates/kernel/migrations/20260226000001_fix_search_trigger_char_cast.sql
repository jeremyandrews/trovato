-- Fix search trigger: cast weight to "char" (internal type) not char (character(1)).
-- setweight() requires "char", not character(1).
CREATE OR REPLACE FUNCTION item_search_update() RETURNS trigger AS $$
DECLARE
    config RECORD;
    vector tsvector := ''::tsvector;
    field_value TEXT;
BEGIN
    -- Always index title as weight A (highest relevance)
    vector := setweight(to_tsvector('english', COALESCE(NEW.title, '')), 'A');

    -- Index configured fields from search_field_config
    FOR config IN
        SELECT field_name, weight
        FROM search_field_config
        WHERE bundle = NEW.type
    LOOP
        -- Extract field value from JSONB
        -- Handle both {field_name: {value: "..."}} and {field_name: "..."} formats
        field_value := COALESCE(
            NEW.fields->config.field_name->>'value',
            NEW.fields->>config.field_name
        );

        IF field_value IS NOT NULL AND field_value != '' THEN
            vector := vector || setweight(
                to_tsvector('english', field_value),
                config.weight::"char"
            );
        END IF;
    END LOOP;

    NEW.search_vector := vector;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
