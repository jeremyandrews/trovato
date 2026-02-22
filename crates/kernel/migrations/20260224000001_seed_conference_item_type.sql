-- Seed "conference" item type for Ritrovo (Story 29.1)
-- Defines all fields from the Ritrovo content model.
-- The conference name maps to the built-in item.title column.

INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings)
VALUES (
    'conference',
    'Conference',
    'A tech conference or meetup event',
    true,
    'Conference Name',
    'ritrovo',
    '{
        "fields": [
            {
                "field_name": "field_url",
                "field_type": {"Text": {"max_length": 2048}},
                "label": "Conference Website",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_start_date",
                "field_type": "Date",
                "label": "Start Date",
                "required": true,
                "cardinality": 1
            },
            {
                "field_name": "field_end_date",
                "field_type": "Date",
                "label": "End Date",
                "required": true,
                "cardinality": 1
            },
            {
                "field_name": "field_city",
                "field_type": {"Text": {"max_length": 255}},
                "label": "City",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_country",
                "field_type": {"Text": {"max_length": 255}},
                "label": "Country",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_online",
                "field_type": "Boolean",
                "label": "Online Event",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_cfp_url",
                "field_type": {"Text": {"max_length": 2048}},
                "label": "CFP URL",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_cfp_end_date",
                "field_type": "Date",
                "label": "CFP End Date",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_description",
                "field_type": "TextLong",
                "label": "Description",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_topics",
                "field_type": {"RecordReference": "category_term"},
                "label": "Topics",
                "required": false,
                "cardinality": -1
            },
            {
                "field_name": "field_logo",
                "field_type": "File",
                "label": "Logo",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_venue_photos",
                "field_type": "File",
                "label": "Venue Photos",
                "required": false,
                "cardinality": -1
            },
            {
                "field_name": "field_schedule_pdf",
                "field_type": "File",
                "label": "Schedule PDF",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_speakers",
                "field_type": {"RecordReference": "speaker"},
                "label": "Speakers",
                "required": false,
                "cardinality": -1
            },
            {
                "field_name": "field_language",
                "field_type": {"Text": {"max_length": 10}},
                "label": "Language",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_source_id",
                "field_type": {"Text": {"max_length": 255}},
                "label": "Source ID",
                "required": false,
                "cardinality": 1
            },
            {
                "field_name": "field_editor_notes",
                "field_type": "TextLong",
                "label": "Editor Notes",
                "required": false,
                "cardinality": 1
            }
        ]
    }'::jsonb
);
