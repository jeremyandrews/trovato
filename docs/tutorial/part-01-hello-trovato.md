# Part 1: Hello, Trovato

Welcome to the Ritrovo tutorial. Over the next eight parts you will build a fully functional tech conference aggregator using Trovato. By the end of Part 1 you will have a running Trovato instance with a `conference` content type, a handful of manually created conferences, and a Gather listing that displays them.

---

## Step 1: Installation

*Coming soon -- covers installing Trovato, running migrations, and verifying the health check.*

---

## Step 2: Define the Conference Item Type

Every piece of content in Trovato is an **Item**. Items are typed -- a blog post, a page, and a conference are all Items, but each has its own set of fields. The blueprint that describes which fields an Item has is called an **Item Type**.

Trovato ships with one built-in Item Type: `page` (a simple page with a body field). Ritrovo needs a `conference` type with fields for dates, location, CFP info, topics, files, and more.

### What Is an Item Type?

An Item Type is a row in the `item_type` table. It stores:

- **type** -- A machine name (lowercase, no spaces). This is the primary key.
- **label** -- A human-readable name shown in the admin UI.
- **description** -- A short explanation for content editors.
- **has_title / title_label** -- Whether items of this type have a title field, and what to call it. Conferences use "Conference Name" instead of the default "Title".
- **plugin** -- Which plugin owns this type. Core types use `core`; Ritrovo types use `ritrovo`.
- **settings** -- A JSONB column containing the field definitions.

### Field Types

Trovato supports these field types:

| FieldType | Description | Example |
|---|---|---|
| `Text` | Short text with optional max length | City name, URL, language code |
| `TextLong` | Long text (rich text with a format like `filtered_html`) | Description, editor notes |
| `Date` | A calendar date | Start date, CFP deadline |
| `Boolean` | True/false | "Is this an online event?" |
| `File` | A file upload (image, PDF, etc.) | Conference logo, schedule PDF |
| `RecordReference` | A link to another record (item, category term, etc.) | Topics, speakers |
| `Integer` | Whole number | Attendee count |
| `Float` | Decimal number | Rating score |
| `Email` | Email address | Contact email |
| `Compound` | A structured field containing sub-fields | Multi-section layouts |

### The Conference Fields

The `conference` Item Type defines 17 fields (the conference name is handled by the built-in `title` column on every Item):

| Field | Type | Required | Multi-value | Purpose |
|---|---|---|---|---|
| `field_url` | Text (max 2048) | no | no | Conference website URL |
| `field_start_date` | Date | **yes** | no | When it starts |
| `field_end_date` | Date | **yes** | no | When it ends |
| `field_city` | Text (max 255) | no | no | City (blank for online-only) |
| `field_country` | Text (max 255) | no | no | Country |
| `field_online` | Boolean | no | no | Whether it is online |
| `field_cfp_url` | Text (max 2048) | no | no | Call for Papers URL |
| `field_cfp_end_date` | Date | no | no | CFP deadline |
| `field_description` | TextLong | no | no | Rich-text description |
| `field_topics` | RecordReference (category_term) | no | **yes** | Topic categories |
| `field_logo` | File | no | no | Conference logo image |
| `field_venue_photos` | File | no | **yes** | Venue/event photos |
| `field_schedule_pdf` | File | no | no | Schedule as PDF |
| `field_speakers` | RecordReference (speaker) | no | **yes** | Linked speaker profiles |
| `field_language` | Text (max 10) | no | no | ISO 639-1 language code |
| `field_source_id` | Text (max 255) | no | no | Dedup key for imports |
| `field_editor_notes` | TextLong | no | no | Internal notes for editors |

### Required vs. Optional

Only `field_start_date` and `field_end_date` are required. Everything else is optional so that conferences can be created incrementally -- an importer might supply just the name, dates, and URL, with editors enriching the record later.

### Multi-Value Fields (Cardinality)

Most fields have a cardinality of 1 (single value). Four fields use cardinality -1, meaning they accept an unlimited number of values:

- **field_topics** -- a conference can span multiple topics (Rust, WebAssembly, Systems)
- **field_venue_photos** -- a gallery of event photos
- **field_speakers** -- many speakers per conference

Multi-value fields are stored as JSON arrays in the JSONB `fields` column on the `item` table.

### Creating the Type

Trovato offers two ways to create an Item Type:

1. **Admin UI** -- Navigate to `/admin/structure/types/add`, fill in the form, then add fields one by one at `/admin/structure/types/conference/fields`.
2. **SQL migration** -- Insert directly into the `item_type` table. This is what Ritrovo does so the type is reproducible and version-controlled.

The Ritrovo migration lives at `crates/kernel/migrations/20260224000001_seed_conference_item_type.sql`. When you run `sqlx migrate run` (or start the server, which runs pending migrations automatically), the `conference` type is created.

After the migration runs, visit `/admin/structure/types` in your browser. You should see "Conference" listed alongside "Basic Page". Click it to inspect the field definitions.

<details>
<summary>Under the Hood: JSONB Field Storage</summary>

The `item_type.settings` column stores field definitions as a JSONB object with a `fields` array. Each entry is a `FieldDefinition`:

```json
{
  "fields": [
    {
      "field_name": "field_start_date",
      "field_type": "Date",
      "label": "Start Date",
      "required": true,
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
      "field_name": "field_city",
      "field_type": {"Text": {"max_length": 255}},
      "label": "City",
      "required": false,
      "cardinality": 1
    }
  ]
}
```

Notice the three shapes of `field_type`:

- **Unit variants** like `Date`, `Boolean`, `File`, `TextLong` serialize as a plain JSON string: `"Date"`.
- **Newtype variants** like `RecordReference("category_term")` serialize as `{"RecordReference": "category_term"}`.
- **Struct variants** like `Text { max_length: Some(255) }` serialize as `{"Text": {"max_length": 255}}`.

This matches Rust's default serde externally-tagged enum serialization. When the kernel boots, it deserializes these definitions into `FieldDefinition` structs (defined in `crates/plugin-sdk/src/types.rs`) and registers them in the `ContentTypeRegistry`.

Actual item data is stored in the `item.fields` JSONB column -- not in `item_type.settings`. The `item_type` defines the schema; `item.fields` holds the values. This separation means you can add or remove fields from a type without migrating existing item data.

</details>

---

## Step 3: Create Your First Conference

*Coming in the next section -- covers creating a conference item via the admin UI and inspecting the stored JSONB.*

---

## Step 4: Build a Gather Listing

*Coming soon -- covers defining a Gather to list upcoming conferences.*
