-- Fix scheduled_publishing gather query field name.
-- The original migration used raw SQL operator syntax "fields->>'field_publish_on'"
-- instead of the proper JSONB path format "fields.field_publish_on" that the
-- GatherQueryBuilder expects.

UPDATE gather_query
SET definition = jsonb_set(
    definition,
    '{filters,0,field}',
    '"fields.field_publish_on"'
),
changed = EXTRACT(EPOCH FROM NOW())::bigint
WHERE query_id = 'scheduled_items'
  AND definition #>> '{filters,0,field}' LIKE 'fields->>%';
