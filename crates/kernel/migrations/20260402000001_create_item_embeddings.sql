-- Item embeddings for semantic search via pgvector.
--
-- This migration checks for the pgvector extension and only creates the
-- table if it is available. When pgvector is not installed, the table is
-- not created and embedding features are gracefully disabled at runtime.

DO $$
BEGIN
    -- Try to enable the extension (requires superuser or CREATE on schema).
    -- If it fails, the table is simply not created.
    CREATE EXTENSION IF NOT EXISTS vector;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'pgvector extension not available — embedding features disabled';
END;
$$;

-- Only create the table if the vector type exists.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_type WHERE typname = 'vector') THEN
        CREATE TABLE IF NOT EXISTS item_embeddings (
            id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            item_id     UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
            field_name  VARCHAR(128) NOT NULL,
            model       VARCHAR(128) NOT NULL,
            dimensions  INT NOT NULL,
            embedding   vector NOT NULL,
            created_at  BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM now())::BIGINT,

            UNIQUE(item_id, field_name, model)
        );

        -- Index for similarity search filtered by model.
        CREATE INDEX IF NOT EXISTS idx_item_embeddings_model
            ON item_embeddings(model);

        -- IVFFlat index for approximate nearest-neighbor search.
        -- Requires at least a few hundred rows to be effective; the index
        -- is created with lists=10 as a starting point and can be rebuilt
        -- with more lists as the dataset grows.
        -- CREATE INDEX IF NOT EXISTS idx_item_embeddings_ivfflat
        --     ON item_embeddings USING ivfflat (embedding vector_cosine_ops) WITH (lists = 10);

        RAISE NOTICE 'item_embeddings table created with pgvector support';
    ELSE
        RAISE NOTICE 'pgvector not available — item_embeddings table not created';
    END IF;
END;
$$;
