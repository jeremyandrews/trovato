# Installing Trovato CMS

This guide walks through a fresh installation of Trovato CMS from source.

## Prerequisites

- **Rust** (stable toolchain, 2024 edition): install via [rustup](https://rustup.rs/)
- **PostgreSQL 15+**: running and accessible
- **Redis 7+**: running on default port (6379)

Verify your toolchain:

```bash
rustc --version   # 1.85+ recommended
psql --version    # 15+
redis-cli ping    # should print PONG
```

## 1. Create the Database

```bash
psql -c "CREATE USER trovato WITH PASSWORD 'trovato';"
psql -c "CREATE DATABASE trovato OWNER trovato;"
```

Or, if your PostgreSQL is configured for peer/trust authentication:

```bash
createdb trovato
```

## 2. Configure Environment

Copy the example environment file and edit as needed:

```bash
cp .env.example .env
```

The defaults work for local development:

```env
PORT=3000
DATABASE_URL=postgres://trovato:trovato@localhost:5432/trovato
REDIS_URL=redis://127.0.0.1:6379
DATABASE_MAX_CONNECTIONS=10
RUST_LOG=info,tower_http=debug,sqlx=warn
```

### Optional Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `PLUGINS_DIR` | `./plugins` | Path to plugin directory |
| `UPLOADS_DIR` | `./uploads` | Path for file uploads |
| `TEMPLATES_DIR` | `./templates` | Tera template directory |
| `CORS_ALLOWED_ORIGINS` | `*` | Comma-separated allowed origins |
| `COOKIE_SAME_SITE` | `strict` | Cookie SameSite policy (`strict`, `lax`, `none`) |
| `JWT_SECRET` | *(none)* | Required for OAuth2 plugin (min 32 bytes) |
| `WEBHOOK_ENCRYPTION_KEY` | *(none)* | Encrypts webhook secrets (min 32 bytes, recommended) |

## 3. Build

```bash
cargo build --release
```

This compiles the kernel binary. Plugin WASM modules ship pre-compiled in
the `plugins/` directory and do not need to be built separately.

## 4. Plugin Selection

Trovato discovers plugins automatically from the `plugins/` directory at
startup. Every plugin found on disk is auto-installed and enabled on first
run.

**To exclude a plugin**, move its directory out of `plugins/` before the
first run:

```bash
mkdir -p plugins-disabled
mv plugins/some_plugin plugins-disabled/
```

**To re-enable a disabled plugin**, move it back and restart:

```bash
mv plugins-disabled/some_plugin plugins/
```

You can also manage plugins after installation via the CLI:

```bash
# List all plugins and their status
cargo run --release -- plugin list

# Disable a plugin
cargo run --release -- plugin disable some_plugin

# Enable a plugin
cargo run --release -- plugin enable some_plugin
```

### Included Plugins

| Plugin | Description |
|--------|-------------|
| `blog` | Blog content type with tags |
| `categories` | Hierarchical taxonomy with tags |
| `comments` | Threaded comments on content |
| `media` | Media library and file management |
| `redirects` | URL redirect management |
| `audit_log` | Administrative audit trail |
| `scheduled_publishing` | Publish/unpublish content on a schedule |
| `content_locking` | Pessimistic content editing locks |
| `webhooks` | Outgoing webhook notifications |
| `image_styles` | Server-side image derivative generation |
| `oauth2` | OAuth2 authorization server (requires `JWT_SECRET`) |
| `locale` | Interface translation |
| `content_translation` | Translatable content fields |
| `config_translation` | Translatable configuration |

### Specialized Plugins (separate install)

The `plugins-disabled/` directory may contain plugins that require
additional infrastructure (e.g., external APIs or dedicated databases).
These are not loaded by default:

- `argus` &mdash; Drupal 6 site monitoring
- `netgrasp` &mdash; Network device tracking
- `goose` &mdash; Load testing integration

## 5. Start the Server

```bash
cargo run --release
```

On first startup, Trovato will:

1. Connect to PostgreSQL and Redis
2. Run database migrations automatically
3. Discover and compile plugins from `plugins/`
4. Run plugin migrations in dependency order
5. Start listening on `http://localhost:PORT`

## 6. Web Installer

Open your browser to `http://localhost:3000`. You will be redirected to
the installation wizard.

**Step 1 &mdash; Welcome:** Confirms PostgreSQL and Redis are connected and
migrations have run.

**Step 2 &mdash; Create Admin Account:** Set your admin username, email
address, and password (minimum 8 characters).

**Step 3 &mdash; Site Configuration:** Set your site name, slogan, and
contact email.

**Step 4 &mdash; Complete:** Installation is finished. Follow the links to
visit your site or the admin dashboard.

## 7. Verify

After installation, confirm everything is running:

```bash
# Health check
curl http://localhost:3000/health

# Admin dashboard (requires login)
open http://localhost:3000/admin
```

## Subsequent Starts

After the initial install, simply run:

```bash
cargo run --release
```

The installer will not run again. Any new database or plugin migrations
are applied automatically on startup.

## Configuration Export/Import

Trovato supports exporting and importing site configuration as YAML files:

```bash
# Export all config to ./config/
cargo run --release -- config export

# Import config from ./config/
cargo run --release -- config import

# Dry-run import (validate without writing)
cargo run --release -- config import --dry-run
```

## Troubleshooting

**Server fails to start with "failed to initialize application state":**
Check that PostgreSQL and Redis are running and your `DATABASE_URL` and
`REDIS_URL` are correct. Look at the full error chain in the log output
for specifics.

**Plugin fails to load:**
Each plugin requires a `{name}.wasm` file matching the `name` field in
its `{name}.info.toml`. Verify the WASM file exists and is named
correctly:

```bash
ls plugins/blog/blog.wasm          # should exist
ls plugins/blog/blog.info.toml     # should exist
```

**OAuth2 not working:**
The OAuth2 plugin requires `JWT_SECRET` to be set in your `.env` with a
value of at least 32 bytes. Without it, the plugin silently skips
initialization.

**Template rendering errors:**
Ensure `TEMPLATES_DIR` points to the `templates/` directory at the
project root. If running from a different working directory, set the
absolute path.
