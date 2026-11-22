-- Text formats as stored configuration (B108).
--
-- The three formats were defined in code (`content::filter`), so a site could
-- not change which tags `filtered_html` allows or add a format of its own. They
-- are rows now, seeded below with exactly what the code defined, so every
-- existing site renders the same output after this migration as before it.
--
-- `filter` is how text is cleaned: `escape` escapes every character HTML gives
-- meaning to, `html` keeps only `allowed_tags` with their `allowed_attributes`
-- (through ammonia), `none` keeps the text as written. After it, `convert_newlines`
-- turns line breaks into `<br>` and `convert_urls` links bare URLs.
--
-- Who may use a format is the permission `use <format>`, as it always was for
-- `filtered_html` and `full_html`; `plain_text` is open to everyone. The three
-- seeded formats cannot be deleted.

CREATE TABLE text_format (
    format VARCHAR(64) PRIMARY KEY CHECK (format ~ '^[a-z][a-z0-9_]*$'),
    name VARCHAR(255) NOT NULL,
    weight INTEGER NOT NULL DEFAULT 0,
    filter VARCHAR(16) NOT NULL CHECK (filter IN ('escape', 'html', 'none')),
    allowed_tags JSONB NOT NULL DEFAULT '[]'::jsonb,
    allowed_attributes JSONB NOT NULL DEFAULT '{}'::jsonb,
    convert_newlines BOOLEAN NOT NULL DEFAULT FALSE,
    convert_urls BOOLEAN NOT NULL DEFAULT FALSE,
    -- Unix seconds.
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);

INSERT INTO text_format
    (format, name, weight, filter, allowed_tags, allowed_attributes,
     convert_newlines, convert_urls, created, changed)
VALUES
    ('filtered_html', 'Filtered HTML', 0, 'html',
     '["p", "br", "strong", "b", "em", "i", "u", "s", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "a", "blockquote", "pre", "code", "table", "thead", "tbody", "tr", "th", "td", "img", "hr"]'::jsonb,
     '{"a": ["href", "title", "target"], "img": ["src", "alt", "title", "width", "height"], "td": ["colspan", "rowspan"], "th": ["colspan", "rowspan"]}'::jsonb,
     FALSE, TRUE,
     EXTRACT(EPOCH FROM NOW())::BIGINT, EXTRACT(EPOCH FROM NOW())::BIGINT),
    ('full_html', 'Full HTML', 1, 'none', '[]'::jsonb, '{}'::jsonb,
     FALSE, FALSE,
     EXTRACT(EPOCH FROM NOW())::BIGINT, EXTRACT(EPOCH FROM NOW())::BIGINT),
    ('plain_text', 'Plain Text', 2, 'escape', '[]'::jsonb, '{}'::jsonb,
     TRUE, FALSE,
     EXTRACT(EPOCH FROM NOW())::BIGINT, EXTRACT(EPOCH FROM NOW())::BIGINT)
ON CONFLICT (format) DO NOTHING;
