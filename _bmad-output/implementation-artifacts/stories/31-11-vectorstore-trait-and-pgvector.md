# Story 31.11: VectorStore Trait & pgvector

Status: ready-for-dev

## Story

As a **site builder**,
I want semantic search powered by vector embeddings alongside keyword search,
so that visitors can find content by meaning ("conferences about reliable distributed systems") even when exact keywords don't match.

## Acceptance Criteria

1. **AC1: VectorStore Trait** — A `VectorStore` trait defines the interface for embedding storage and similarity search: `store(id, embedding, metadata)`, `search(query_embedding, limit, filter) -> Vec<ScoredResult>`, `delete(id)`, `count()`. The trait is async and object-safe for dynamic dispatch.

2. **AC2: pgvector Implementation** — A `PgVectorStore` implementation uses the `pgvector` PostgreSQL extension for embedding storage. Table schema: `id (UUID)`, `item_id (UUID, FK)`, `embedding (vector)`, `metadata (JSONB)`, `created (bigint)`. Uses `<=>` cosine distance operator for similarity search.

3. **AC3: Migration** — Database migration enables the `pgvector` extension (`CREATE EXTENSION IF NOT EXISTS vector`) and creates the embeddings table with appropriate indexes (IVFFlat or HNSW for approximate nearest neighbor search).

4. **AC4: Embedding Generation** — Integration with `AiProviderService` to generate embeddings via the `Embedding` operation type. Items are embedded on create/update when embedding is configured for the content type.

5. **AC5: Search Integration** — `SearchService` extended with a `semantic_search()` method that: (a) generates an embedding for the query text, (b) queries the vector store for nearest neighbors, (c) returns results with similarity scores. Can be combined with keyword search for hybrid ranking.

6. **AC6: Configuration** — Per-content-type embedding configuration: which fields to embed, embedding model, dimension size. Stored in content type definition or site config.

7. **AC7: Indexing Pipeline** — Background embedding generation for existing content. Items queued for embedding on save via `tap_item_update_index` or a dedicated embedding queue. Rate-limited to stay within AI provider token budgets.

8. **AC8: Integration Tests** — Tests verify: (a) VectorStore trait implementations pass a standard test suite; (b) pgvector store/search/delete operations work correctly; (c) embedding generation produces correct-dimension vectors; (d) semantic search returns relevant results ranked by similarity.

## Tasks / Subtasks

- [ ] Task 1: Design VectorStore trait (AC: #1)
  - [ ] 1.1 Define `VectorStore` trait in `crates/kernel/src/services/` or `crates/kernel/src/search/`
  - [ ] 1.2 Define `ScoredResult` return type with id, score, metadata
  - [ ] 1.3 Define `VectorFilter` for metadata-based filtering
- [ ] Task 2: Create pgvector migration (AC: #3)
  - [ ] 2.1 Create migration enabling `pgvector` extension
  - [ ] 2.2 Create `item_embeddings` table with vector column
  - [ ] 2.3 Create HNSW or IVFFlat index for approximate nearest neighbor
- [ ] Task 3: Implement PgVectorStore (AC: #2)
  - [ ] 3.1 Implement `store()` — upsert embedding with metadata
  - [ ] 3.2 Implement `search()` — cosine similarity query with filters
  - [ ] 3.3 Implement `delete()` — remove embedding by item ID
  - [ ] 3.4 Implement `count()` — count stored embeddings
- [ ] Task 4: Embedding generation (AC: #4)
  - [ ] 4.1 Create embedding generation function using AiProviderService Embedding operation
  - [ ] 4.2 Concatenate configured fields into embedding input text
  - [ ] 4.3 Handle provider response with vector extraction
- [ ] Task 5: Search integration (AC: #5)
  - [ ] 5.1 Add `semantic_search()` to SearchService
  - [ ] 5.2 Implement hybrid search combining keyword + semantic scores
  - [ ] 5.3 Add semantic search option to search API endpoint
- [ ] Task 6: Configuration (AC: #6)
  - [ ] 6.1 Define per-content-type embedding config schema
  - [ ] 6.2 Store in site config or content type definition
  - [ ] 6.3 Admin UI for enabling/configuring embeddings per content type
- [ ] Task 7: Indexing pipeline (AC: #7)
  - [ ] 7.1 Embed on item create/update (after save)
  - [ ] 7.2 Background queue for bulk re-embedding
  - [ ] 7.3 Rate limiting to respect token budgets
- [ ] Task 8: Integration tests (AC: #8)

## Dev Notes

### Design Considerations

The `VectorStore` trait should be abstract enough to support future backends (Qdrant, Pinecone, etc.) while the initial implementation uses pgvector for simplicity (no additional infrastructure).

### pgvector Setup

```sql
-- Migration
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE item_embeddings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
    embedding vector(1536),  -- dimension depends on model
    metadata JSONB DEFAULT '{}',
    created BIGINT NOT NULL DEFAULT extract(epoch from now())::bigint,
    UNIQUE(item_id)
);

-- HNSW index for fast approximate nearest neighbor search
CREATE INDEX idx_item_embeddings_hnsw ON item_embeddings
    USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);
```

### Embedding Dimension

Depends on the configured model:
- OpenAI `text-embedding-3-small`: 1536 dimensions
- OpenAI `text-embedding-3-large`: 3072 dimensions
- Anthropic (via Voyage AI): 1024 dimensions
- Local models vary

The vector column dimension should be configurable or the table should store the dimension as metadata.

### Hybrid Search Strategy

Combine keyword (PostgreSQL FTS `ts_rank`) and semantic (cosine similarity) scores:
```
final_score = alpha * keyword_score + (1 - alpha) * semantic_score
```
Where `alpha` is configurable (default 0.5). Keyword search provides exact match precision; semantic search provides conceptual recall.

### Key Dependencies

- `pgvector` crate for Rust pgvector support with sqlx
- `pgvector` PostgreSQL extension (must be installed on the database server)

### Key Files (Planned)

- `crates/kernel/src/search/vector.rs` — VectorStore trait + PgVectorStore implementation
- `crates/kernel/migrations/NNNN_create_item_embeddings.sql` — pgvector extension + table
- `crates/kernel/src/search/mod.rs` — Extended with semantic_search()
- `crates/kernel/src/services/ai_provider.rs` — Embedding operation already supported
