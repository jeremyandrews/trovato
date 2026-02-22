-- Seed 3 conference items for Ritrovo tutorial (Story 29.2)
-- Uses the anonymous user (nil UUID) as author.
-- Idempotent: checks each conference independently before inserting.
-- Generates UUIDv7 IDs (time-sorted) to match the kernel's Rust-side behavior.
-- Boolean fields use string "1" to match form submission format (not JSON true/false).

-- Session-scoped UUIDv7 generator (RFC 9562 §5.7)
CREATE OR REPLACE FUNCTION pg_temp.uuid_v7() RETURNS uuid AS $$
DECLARE
    v_ts bigint;
    v_bytes bytea;
BEGIN
    v_ts := (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint;
    -- 6 bytes of millisecond timestamp + 10 random bytes
    v_bytes := substring(int8send(v_ts) from 3) || gen_random_bytes(10);
    -- Set version nibble to 7
    v_bytes := set_byte(v_bytes, 6, (get_byte(v_bytes, 6) & x'0f'::int) | x'70'::int);
    -- Set variant bits to RFC 4122 (10xx)
    v_bytes := set_byte(v_bytes, 8, (get_byte(v_bytes, 8) & x'3f'::int) | x'80'::int);
    RETURN encode(v_bytes, 'hex')::uuid;
END$$ LANGUAGE plpgsql;

DO $$
DECLARE
    v_id UUID;
    v_rev UUID;
    v_author UUID := '00000000-0000-0000-0000-000000000000';
    v_now BIGINT := EXTRACT(EPOCH FROM now())::bigint;
BEGIN
    -- 1. RustConf 2026 — Portland, OR, USA
    IF NOT EXISTS (SELECT 1 FROM item WHERE type = 'conference' AND title = 'RustConf 2026') THEN
        v_id  := pg_temp.uuid_v7();
        v_rev := pg_temp.uuid_v7();

        INSERT INTO item (id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
        VALUES (
            v_id, 'conference', 'RustConf 2026', v_author, 1, v_now, v_now, 0, 0,
            '{
                "field_url": "https://rustconf.com",
                "field_start_date": "2026-09-09",
                "field_end_date": "2026-09-11",
                "field_city": "Portland",
                "field_country": "United States",
                "field_cfp_url": "https://rustconf.com/cfp",
                "field_cfp_end_date": "2026-06-15",
                "field_description": "The official Rust conference, featuring talks on the latest Rust developments.",
                "field_language": "en"
            }'::jsonb,
            'live'
        );

        INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log)
        VALUES (v_rev, v_id, v_author, 'RustConf 2026', 1,
                (SELECT fields FROM item WHERE id = v_id), v_now, 'Initial seed');

        UPDATE item SET current_revision_id = v_rev WHERE id = v_id;

        INSERT INTO url_alias (source, alias, created) VALUES ('/item/' || v_id::text, '/item/' || v_id::text, v_now);
    END IF;

    -- 2. EuroRust 2026 — Paris, France
    IF NOT EXISTS (SELECT 1 FROM item WHERE type = 'conference' AND title = 'EuroRust 2026') THEN
        v_id  := pg_temp.uuid_v7();
        v_rev := pg_temp.uuid_v7();

        INSERT INTO item (id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
        VALUES (
            v_id, 'conference', 'EuroRust 2026', v_author, 1, v_now, v_now, 0, 0,
            '{
                "field_url": "https://eurorust.eu",
                "field_start_date": "2026-10-15",
                "field_end_date": "2026-10-16",
                "field_city": "Paris",
                "field_country": "France",
                "field_description": "Europe''s premier Rust conference, bringing together Rustaceans from across the continent.",
                "field_language": "en"
            }'::jsonb,
            'live'
        );

        INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log)
        VALUES (v_rev, v_id, v_author, 'EuroRust 2026', 1,
                (SELECT fields FROM item WHERE id = v_id), v_now, 'Initial seed');

        UPDATE item SET current_revision_id = v_rev WHERE id = v_id;

        INSERT INTO url_alias (source, alias, created) VALUES ('/item/' || v_id::text, '/item/' || v_id::text, v_now);
    END IF;

    -- 3. WasmCon Online 2026 — online-only
    IF NOT EXISTS (SELECT 1 FROM item WHERE type = 'conference' AND title = 'WasmCon Online 2026') THEN
        v_id  := pg_temp.uuid_v7();
        v_rev := pg_temp.uuid_v7();

        INSERT INTO item (id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id)
        VALUES (
            v_id, 'conference', 'WasmCon Online 2026', v_author, 1, v_now, v_now, 0, 0,
            '{
                "field_url": "https://wasmcon.dev",
                "field_start_date": "2026-07-22",
                "field_end_date": "2026-07-23",
                "field_online": "1",
                "field_description": "A virtual conference dedicated to WebAssembly, covering toolchains, runtimes, and the component model.",
                "field_language": "en"
            }'::jsonb,
            'live'
        );

        INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log)
        VALUES (v_rev, v_id, v_author, 'WasmCon Online 2026', 1,
                (SELECT fields FROM item WHERE id = v_id), v_now, 'Initial seed');

        UPDATE item SET current_revision_id = v_rev WHERE id = v_id;

        INSERT INTO url_alias (source, alias, created) VALUES ('/item/' || v_id::text, '/item/' || v_id::text, v_now);
    END IF;
END $$;
