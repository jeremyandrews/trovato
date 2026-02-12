# Trovato

Trovato is a content management system built in Rust, reimagining Drupal 6's elegant mental model with modern foundations. The core insight that made Drupal 6 powerful—everything is a node, fields bolt on via CCK, Views queries anything, hooks enable extensibility—remains valuable, but the implementation deserves a fresh start. Trovato rebuilds these ideas with Axum and Tokio for async HTTP, PostgreSQL with hybrid relational/JSONB storage to eliminate field-join complexity, and WebAssembly plugins that run in per-request sandboxes via Wasmtime.

Security and performance drive the architecture. Plugins are treated as untrusted code: they run in WASM sandboxes, return structured JSON render trees rather than raw HTML, and access data through a handle-based API that avoids serialization overhead. The kernel sanitizes all output before rendering via Tera templates. A structured database interface prevents SQL injection at the boundary, and explicit capability grants control what each plugin can access. This isn't security theater—the WASM boundary enforces isolation whether plugin authors intend it or not.

The system is designed for horizontal scaling from day one. No persistent state lives in the binary; everything goes to PostgreSQL or Redis. Content staging ("Stages") is baked into the schema rather than bolted on later. The plugin SDK prioritizes developer ergonomics—write the code you want plugin authors to write, then build the host to support it. Trovato carries forward the ideas that made Drupal great while taking full advantage of Rust's safety guarantees and modern async runtimes.

---

*This project is being developed with AI assistance.*
