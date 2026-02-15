# Headless Frontend Example

A minimal single-page app that consumes the Trovato CMS JSON API from a
separate origin, demonstrating CORS, Bearer-token auth, content listing,
search, and comment posting.

## Prerequisites

- A running Trovato instance (default `http://localhost:3000`)
- Python 3 (for the dev server) or any static file server

## Quick Start

1. **Configure the API base URL** — open `index.html` and edit `API_BASE` if
   your Trovato instance is not at `http://localhost:3000`.

2. **Start the dev server** on a different port so CORS is exercised:

   ```sh
   cd examples/headless-frontend
   python3 -m http.server 8080
   ```

3. **Open** `http://localhost:8080` in your browser.

## Authentication

The example supports two auth methods:

- **Session cookie** — use the login form (works when origins share cookies or
  CORS is configured with specific origins + credentials).
- **Bearer token** — paste an API token into the token field. Tokens are
  created via `POST /api/tokens` after logging in.

## What It Demonstrates

- Fetching paginated content via `GET /api/items`
- Content-type filtering via `GET /api/content-types`
- Full-text search via `GET /api/search`
- Single-item detail view via `GET /api/item/{id}`
- Posting comments when authenticated
- Bearer token authentication for cross-origin requests
