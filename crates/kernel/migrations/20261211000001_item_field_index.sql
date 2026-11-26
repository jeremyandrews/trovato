-- B207: indexes on item fields.
--
-- Items keep every field in one JSONB column, `fields`, whose documents are
-- large and stored compressed out of line. A field marked "Filterable and
-- sortable" is also copied, as a bare value, into `filter_values`, a small
-- JSONB column that holds only the marked fields and stays in the row; a
-- background job fills it for existing items, then builds an index and
-- statistics on the field's reader of it (`crates/kernel/src/gather/
-- field_sql.rs`, `docs/design/field-indexes.md`). item_field_index is both
-- the record of those fields and the queue of that job.
--
-- Adding a nullable column without a default does not rewrite the table.
--
-- trovato_field_numeric is the reader of a number field: a field value (the
-- payload of a {"value": ...} wrapper, or the bare value) as numeric, or NULL
-- when it is not a number. IMMUTABLE, so an index can be built on it; a
-- string is converted only when it spells a number, so building an index
-- never fails on a stray value.
CREATE OR REPLACE FUNCTION trovato_field_numeric(v jsonb) RETURNS numeric
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
  SELECT CASE jsonb_typeof(v)
    WHEN 'number' THEN v::numeric
    WHEN 'string' THEN CASE
      WHEN (v #>> '{}') ~ '^\s*[-+]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][-+]?[0-9]+)?\s*$'
      THEN (v #>> '{}')::numeric
    END
  END
$$;

-- trovato_field_list is the indexed form of a list field's copy (a JSON
-- array of ids or values): the same elements as a text[], or NULL when the
-- copy is not an array. A GIN index on it serves `@>`, and statistics on it
-- record the most common elements, which PostgreSQL uses to estimate how
-- many items have a tag; for a jsonb array it has only whole-array
-- statistics, and at a million items the planner then walked every item
-- newest first looking for a rare tag (1.4 s) instead of reading the index
-- (8 ms).
CREATE OR REPLACE FUNCTION trovato_field_list(v jsonb) RETURNS text[]
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
  SELECT CASE jsonb_typeof(v)
    WHEN 'array' THEN ARRAY(SELECT jsonb_array_elements_text(v))
  END
$$;

CREATE TABLE item_field_index (
    -- The item type and field the index is on.
    item_type VARCHAR(32) NOT NULL,
    field_name VARCHAR(64) NOT NULL,
    -- How the field is read: text, number or list (FieldKind).
    kind VARCHAR(16) NOT NULL,
    -- The index's name in pg_class.
    index_name VARCHAR(63) NOT NULL,
    -- queued: waiting to be built; building; ready; failed (see error);
    -- dropping: waiting to be dropped (the row goes when it is).
    state VARCHAR(16) NOT NULL DEFAULT 'queued',
    error TEXT,
    -- Unix seconds.
    requested BIGINT NOT NULL,
    started BIGINT,
    finished BIGINT,
    -- How long the last build took.
    build_ms BIGINT,
    -- While building: what it is doing (filling the copy, indexing) and,
    -- while filling, how many of the type's slices are done, of 256.
    phase VARCHAR(16),
    progress INTEGER,
    -- Whether the copy column holds this field for every existing item: a
    -- fill of the type covers all its marked fields at once, and the jobs of
    -- the others skip theirs.
    filled BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (item_type, field_name)
);

CREATE INDEX idx_item_field_index_state ON item_field_index (state);

ALTER TABLE item ADD COLUMN IF NOT EXISTS filter_values JSONB;

-- Recomputes filter_values from fields on every insert and every update of
-- type or fields. The kernel replaces this body with one that copies each
-- type's marked fields (field_sql::trigger_function_sql) whenever they
-- change; until a field is marked it copies nothing.
CREATE OR REPLACE FUNCTION trovato_item_filter_values() RETURNS trigger
LANGUAGE plpgsql AS $fn$
BEGIN
  NEW.filter_values := NULL;
  RETURN NEW;
END
$fn$;

CREATE TRIGGER item_filter_values
    BEFORE INSERT OR UPDATE OF type, fields ON item
    FOR EACH ROW EXECUTE FUNCTION trovato_item_filter_values();
