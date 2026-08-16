-- Search index trigger function
-- Story 12.3: Search Index Trigger

-- Function to update search_vector on item insert/update
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
                config.weight::char
            );
        END IF;
    END LOOP;

    NEW.search_vector := vector;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Create trigger on item table
DROP TRIGGER IF EXISTS trg_item_search ON item;
CREATE TRIGGER trg_item_search
    BEFORE INSERT OR UPDATE OF title, fields, type ON item
    FOR EACH ROW
    EXECUTE FUNCTION item_search_update();

-- Comment explaining the trigger
COMMENT ON FUNCTION item_search_update() IS 'Updates search_vector tsvector column based on title and configured searchable fields';
