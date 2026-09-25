-- Normalize item_type.settings onto the canonical object shape.
--
-- Two shapes were written to this column. The core seed migration and every
-- admin-side writer store `{"fields": [...]}`; plugin registration stored the
-- bare array `[...]`. The reader (`parse_fields_from_settings`) only ever
-- understood the object shape, so every plugin-declared content type loaded
-- back with zero fields: no field inputs in the admin form, and no required
-- field validation on save.
--
-- The writer now emits the object shape. This repairs the rows already written
-- in the array shape, including types whose plugin is currently disabled or
-- uninstalled and so will never be re-registered.
UPDATE item_type
SET settings = jsonb_build_object('fields', settings)
WHERE jsonb_typeof(settings) = 'array';

-- Anything still not an object (a scalar, or SQL NULL from before the column
-- had a default) carries no field definitions at all. `jsonb_typeof(NULL)` is
-- NULL, so this needs IS DISTINCT FROM rather than <>. Normalizing these too
-- lets every reader and writer assume an object from here on.
UPDATE item_type
SET settings = '{}'::jsonb
WHERE jsonb_typeof(settings) IS DISTINCT FROM 'object';
