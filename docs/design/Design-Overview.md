# Trovato: Complete Design Document (v2.1)

**Rebuilding Drupal 6 in Rust**

*February 2026 — Tag1 Consulting*

---

## What This Is

This document describes how to build a content management system in Rust that replicates the core behavior of Drupal 6. Not a port — a reimagining that keeps what made Drupal 6 good (simple mental model, flexible content types, composable plugins) while replacing the implementation with modern, high-performance foundations (Rust, WASM, Actor-like concurrency).

**Version 2.1** incorporates the Pooled Instantiation Model for WASM concurrency, a strict Render Tree pipeline to prevent XSS, Stage-based content staging from day one, and a structured DB API to prevent SQL injection from untrusted plugins.

The target audience is a developer who knows Rust and wants to understand what Drupal 6 actually did under the hood, and how each piece maps to a Rust implementation.

## What Made Drupal 6 Good

If you never used Drupal 6, here's the pitch: everything was a "node." A blog post was an item. A page was an item. A product listing was an item. Each item had a type, and you could bolt on extra fields to any type using CCK (Content Construction Kit). Then you could query those items using Gather — a visual SQL query builder for non-developers. Plugins could tap into any part of the system by implementing specially-named functions. You could build surprisingly complex sites without writing code.

The whole thing ran on PHP, MySQL, and hope. It scaled badly, the security model was an afterthought, and loading a single item with 20 custom fields meant 20 JOINs. But the mental model was elegant.

## What We're Building

A Rust binary (the "Kernel") that:

- Serves HTTP via **Axum** with **Gander-style observability**
- Stores content in **PostgreSQL** using a hybrid relational/JSONB schema with **UUIDv7** primary keys
- Loads plugins as **WebAssembly (WASM)** binaries at runtime (no recompilation)
- Isolates plugin state **per-request** (Pooled Stores) to ensure thread safety
- Uses a **Render Tree** (JSON) pipeline to ensure alterability and security (no opaque HTML strings from plugins)
- Supports dynamic content types with custom fields (CCK equivalent)
- Supports dynamic query building (Gather equivalent)
- Supports **Stages** (content staging) natively
- Manages sessions via **Redis** for multi-server deployments
- Handles file uploads with pluggable storage backends (local/S3)
- Provides full-text search via PostgreSQL tsvector
- Includes categories for content categorization and hierarchical term organization

## What We're Not Building (Yet)

- A visual UI for Gather configuration (we build the query engine; the UI is a later project)
- Multilingual support
- A migration tool from actual Drupal 6 databases (design sketched in Section 17; implementation deferred)
- Draft/published workflow *logic* (the Stage schema supports it, but complex moderation UIs are deferred)
- Revision moderation and approval queues
- Search across *draft* stages (Search is Live-only for MVP)
- CSS/JS aggregation (handle at reverse proxy or build tool level)

---

## Architecture Overview

```
┌─────────────────────────────────────────────┐
│                  NGINX / HAProxy            │
│              (Load Balancer)                │
└──────────────┬──────────────────────────────┘
               │
    ┌──────────▼──────────┐
    │   Rust Binary        │  ◄── The "Kernel"
    │   (Axum + Tokio)     │
    │                      │
    │  ┌────────────────┐  │
    │  │ Profiler        │  │  ◄── Gander Middleware
    │  └────────────────┘  │
    │  ┌────────────────┐  │
    │  │ Tap Dispatcher │  │  ◄── Manages Pooled WASM Stores
    │  └────────────────┘  │
    │  ┌────────────────┐  │
    │  │ Render Engine   │  │  ◄── JSON Render Tree → Tera Templates
    │  └────────────────┘  │
    │  ┌────────────────┐  │
    │  │ Record System   │  │  ◄── Item/User CRUD + Stages
    │  └────────────────┘  │
    │  ┌────────────────┐  │
    │  │ Query Builder   │  │  ◄── Gather engine (SeaQuery)
    │  └────────────────┘  │
    │  ┌────────────────┐  │
    │  │ Theme Engine    │  │  ◄── Tera templates + suggestions
    │  └────────────────┘  │
    │  ┌────────────────┐  │
    │  │ Router          │  │  ◄── Static + dynamic path resolution
    │  └────────────────┘  │
    └──────┬─────────┬─────┘
           │         │
    ┌──────▼───┐ ┌───▼──────┐
    │ Postgres │ │  Redis   │
    │          │ │          │
    │ - Users  │ │ - Sessions│
    │ - Items  │ │ - Cache   │
    │ - Config │ │ - Locks   │
    │ - Search │ │ - Queues  │
    └──────────┘ └──────────┘

    ┌───────────────────┐
    │  File Storage      │
    │  (Local / S3)      │
    └───────────────────┘
```

**Key Design Rule 1:** The Rust binary holds no persistent state. All state lives in Postgres or Redis. This means you can run multiple instances behind a load balancer without session affinity.

**Key Design Rule 2:** Plugins are untrusted. They interact with the Kernel only through the WASM Interface Type (WIT) boundary.

**Key Design Rule 3:** No opaque HTML strings from plugins. Plugins return structured JSON (Render Elements); the Kernel handles rendering and sanitization.

---

## Detailed Design Documents

This design is split into focused documents for easier navigation:

- [[Projects/Trovato/Design-Web-Layer|Web Layer & Sessions]] — HTTP routing, middleware, sessions, authentication (Sections 1-2)
- [[Projects/Trovato/Design-Plugin-System|Plugin & Tap System]] — WASM plugin loading, tap dispatch, SDK (Sections 3-4)
- [[Projects/Trovato/Design-Render-Theme|Render Tree & Forms]] — JSON render pipeline, Form API (Sections 5, 10)
- [[Projects/Trovato/Design-Content-Model|Content Model]] — Items/CCK, stages, revisions, categories (Sections 6-8)
- [[Projects/Trovato/Design-Query-Engine|Gather Query Engine]] — Dynamic query builder (Section 9)
- [[Projects/Trovato/Design-Infrastructure|Infrastructure]] — Files, cron, search, caching, error handling (Sections 11-15)
- [[Projects/Trovato/Design-Project-Meta|Project Meta]] — Benchmarks, migration, project structure, dependencies, roadmap, decisions, gaps (Sections 16-23)
- [[Projects/Trovato/Design-Plugin-SDK|Plugin SDK Spec]] — Types, macros, host functions, WIT interface, mutation model, complete examples
- [[Projects/Trovato/Terminology|Terminology]] — Drupal → Trovato naming map
