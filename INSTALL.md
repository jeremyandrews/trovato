# Installing Trovato CMS

This guide walks through a fresh installation of Trovato CMS from source.

## Docker Alternative

If you don't have Rust installed, you can use Docker for the full development environment:

```bash
docker compose --profile dev up -d
docker compose exec dev bash
# Full Rust toolchain available inside the container
```

See [Docker Development Guide](docs/docker-development.md) for details.

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
| `PORT` | `3000` | HTTP server port |
| `SITE_URL` | `http://localhost:<PORT>` | The site's public base URL. Every absolute address the kernel emits is built from it: `sitemap.xml`, the `Sitemap:` line in `robots.txt`, the RSS feeds, `<link rel="canonical">`, the Open Graph tags and the links in outgoing mail. Set it to the scheme and host visitors actually use, or a crawler is handed addresses that resolve to a developer's laptop. A trailing slash is ignored. |
| `DATABASE_MAX_CONNECTIONS` | `10` | PostgreSQL connection pool size |
| `RUST_LOG` | `info` | Log filter, in `RUST_LOG` format |
| `PLUGINS_DIR` | `./plugins` | Plugin search path. Several directories may be given, separated by `:`; later ones win a name collision. |
| `UPLOADS_DIR` | `./uploads` | Path for file uploads. The server checks at startup that it can write, read and delete a file there, and refuses to start if it cannot. |
| `FILES_URL` | `/files` | Base URL that uploaded files are served under |
| `TEMPLATES_DIR` | `./templates` | Tera template search path. Several directories may be given, separated by `:`; later ones override templates of the same name. |
| `THEMES_DIR` | `./themes` | Where themes are found, one directory per theme holding `theme.toml` (`name`, `description`, optional `screenshot`), `templates/` laid over the stock templates, and `static/` served at `/static/themes/<name>/`. Several directories may be given, separated by `:`; a later one's theme replaces an earlier one of the same name. Chosen at `/admin/appearance`. |
| `STATIC_DIR` | `./static` | Static asset search path, served under `/static`. Several directories may be given, separated by `:`; later ones override assets of the same path. The generated Pagefind index is written to the first directory unless `PAGEFIND_OUTPUT_DIR` says otherwise. |
| `PAGEFIND_OUTPUT_DIR` | `pagefind` under the first `STATIC_DIR` | Where the Pagefind index is built and served from; its URL stays `/static/pagefind/`. Each rebuild is made beside it and renamed into place, so its parent must be writable and on the same filesystem. On several servers, point it into the shared volume. |
| `CORS_ALLOWED_ORIGINS` | `*` | Comma-separated allowed origins |
| `COOKIE_SAME_SITE` | `strict` | Cookie SameSite policy (`strict`, `lax`, `none`) |
| `COOKIE_SECURE` | `always` | Whether the session cookie is `Secure`: `always` or `never`. Leave it at `always` for any site reached over HTTPS. `never` is for a plain HTTP LAN or development site only. See "Serving over plain HTTP" below. |
| `TRUSTED_PROXIES` | *(empty: trust none)* | Reverse proxies whose `X-Forwarded-For` and `X-Forwarded-Proto` are believed, as comma-separated addresses and CIDR ranges. See "Behind a reverse proxy" below. |
| `CLIENT_IP_STORAGE` | `full` | How much of a client address is stored: `full`, `truncated` or `none`. See "Client addresses and privacy" below. |
| `CRON_KEY` | *(none)* | Shared secret in the cron URL, `POST /cron/<CRON_KEY>`. Change it before exposing the server. |
| `JWT_SECRET` | *(none)* | Required for OAuth2 plugin (min 32 bytes) |
| `WEBHOOK_ENCRYPTION_KEY` | *(none)* | Encrypts webhook secrets (min 32 bytes, recommended) |
| `UPDATE_CHECK` | `1` | Whether the kernel may check for Trovato releases. Set to `0` (or `false`/`off`/`no`) for a deployment that must make no outbound requests at all. See "Update checks" below. |
| `UPDATE_CHECK_INTERVAL_SECS` | `86400` | Minimum seconds between update checks |
| `UPDATE_CHECK_ENDPOINT` | GitHub's latest-release API | The endpoint the check asks. Configurable so a test can point it at a local server. |

`.env.example` is the fuller reference, including the `POSTGRES_*` and
`REDIS_PORT` settings that only `docker compose` reads.

### Serving uploaded files

Never point a web server, reverse proxy static location or CDN origin at
`UPLOADS_DIR`. It holds every upload in one tree, including files attached only
to unpublished or restricted content, and the image style derivatives generated
from them under `UPLOADS_DIR/styles/`. The kernel decides access for each file
against the content that references it, so every file must be fetched through
the kernel's `/files/` routes. A proxy in front of the kernel may cache what the
kernel marks `public`; responses it marks `private, no-store` must not be kept.

The kernel answers `/files/` with the type stored when the upload was validated
and `X-Content-Type-Options: nosniff`; a proxy must pass `Content-Type` through
unchanged. Uploads are limited to 10 MB, and the upload routes accept request
bodies of up to 10 MB plus 64 KiB of multipart framing. A proxy with a smaller
body limit (nginx defaults to 1 MB) must allow at least that on `/file/upload`
and `/api/v1/block-editor/upload`, for example `client_max_body_size 11m;`, or
editors get the proxy's error instead of the kernel's.

### Behind a reverse proxy

Trovato takes the client address from the TCP connection. Behind a reverse
proxy (nginx, Caddy, HAProxy, a Kubernetes ingress, a CDN) that connection comes
from the proxy, so every visitor would share the proxy's address, its rate-limit
budget and its sign-in lockout counter. Tell Trovato which peers are proxies:

```env
TRUSTED_PROXIES=127.0.0.1,::1
```

That is the whole change for nginx on the same host. List every hop in front of
Trovato, as addresses or CIDR ranges: a load balancer behind a CDN is
`TRUSTED_PROXIES=10.0.0.0/8,<the CDN's published ranges>`; a pod behind an
ingress is the cluster's pod network, for example `TRUSTED_PROXIES=10.42.0.0/16`.

When the connection comes from a listed proxy, Trovato reads `X-Forwarded-For`
from the right, skips the entries that are themselves listed proxies, and takes
the first address that is not one. A proxy that sends no `X-Forwarded-For` may
send `X-Real-IP` instead. A connection from anywhere else is the client, and
its forwarding headers are ignored, so a visitor who connects directly cannot
choose their own address by sending one. The RFC 7239 `Forwarded` header is not
read; every common proxy writes `X-Forwarded-For`.

`X-Forwarded-Proto` is believed only from the same listed proxies. It is how
Trovato learns that the visitor connected over HTTPS, and only then does a
response carry `Strict-Transport-Security`. A site behind a TLS-terminating
proxy must list that proxy in `TRUSTED_PROXIES` to send HSTS at all; with
`TRUSTED_PROXIES` unset, no response carries it, whatever the proxy sends. A
visitor who connects directly cannot ask for HSTS by sending the header.
`COOKIE_SECURE` does not depend on it: the session cookie is `Secure` by
setting, not by header (see "Serving over plain HTTP" below).

The proxy must append to `X-Forwarded-For`, not pass the client's value through
unchanged. For nginx:

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Forwarded-Proto $scheme;
```

`TRUSTED_PROXIES` is empty by default, which trusts no proxy. An entry that is
not an address or a CIDR range (`10.0.0.0/33`, a hostname, a typo) stops the
server at startup with a message naming it, instead of being dropped and
silently trusting one proxy fewer.

### Serving over plain HTTP

The session cookie, the only cookie Trovato sets (CSRF tokens and "remember me"
live inside the session), is `Secure` by default. A browser keeps a `Secure`
cookie only from an HTTPS page, with one exception for `localhost`, so a site
served over plain HTTP at a LAN address such as `http://192.168.1.20:3000` or
`http://cms.lan` cannot be logged in to: the login answers, the browser drops
the cookie, and the next page is anonymous again.

For that site, and only that site, set:

```env
COOKIE_SECURE=never
SITE_URL=http://192.168.1.20:3000
```

Use `never` only on a plain HTTP LAN or a development machine, never on a site
reachable from the internet. Without `Secure` the browser sends the session
cookie over any plain HTTP request to the host, so anyone on the network path
(a shared Wi-Fi, a hostile router, an ISP) can read it and act as the signed-in
user, an administrator included, without knowing the password. Behind HTTPS,
either directly or through a TLS-terminating proxy, leave it at `always`, even
when the proxy talks plain HTTP to Trovato: the browser sees HTTPS, and that
is what `Secure` is about.

Guard rails: with `never` the server logs a warning naming `COOKIE_SECURE` on
every start, and the site settings page (`/admin/config/site`) shows it under
"Session cookie". `never` is refused at startup when `SITE_URL` is `https://`,
and when `COOKIE_SAME_SITE=none` (browsers reject a `SameSite=None` cookie that
is not `Secure`). Any value other than `always` or `never` is refused too. There
is no `auto`: the flag is fixed at startup, and a per-request guess from the
request scheme is one forwarded header away from the wrong answer.

### Client addresses and privacy

`CLIENT_IP_STORAGE` decides how much of a client address Trovato keeps once the
request is over:

| Value | What is stored |
|-------|----------------|
| `full` (default) | The whole address, as before this setting existed. |
| `truncated` | The network only: IPv4 to its /24 (`203.0.113.77` is kept as `203.0.113.0`), IPv6 to its /48. |
| `none` | Nothing. |

It applies everywhere an address outlives the request: the `ip_address` column
of the security audit log (`security_audit_log`, NULL under `none`) and of the
audit log plugin's `audit_log` (the empty string under `none`, since that column
cannot be NULL), the client address on sign-in lockout warnings in the site log
(`system_log`), the address shown on a user's device list (the session index in
Redis, kept as long as the session), and the client address in password reset
log lines. Comments, users and items store no client address under any value.

Rate limiting and sign-in lockout always use the whole address, under every
value, because telling clients apart is their whole job. They keep it in Redis
only, as counter keys that expire with their window (a minute to an hour with
the default limits; lockout keys within thirty minutes). Nothing is written to
the database for them.

A site that wants no address on record sets `CLIENT_IP_STORAGE=none` and keeps
its proxy passing the real address in `X-Forwarded-For`; there is no need to
rewrite the header to a constant at the proxy, which would also put every
visitor in one rate-limit budget. An unknown value stops the server at startup.

### Rate Limits

Every request is counted against one bucket, chosen by its path and method. A
request that presents no credential is counted per client IP; a signed-in request
(a session cookie or an API token) is counted per user, at the count of the user's
role. Each bucket's default is the request count in the table below; the window is
fixed per bucket, whoever is counted.

A limit is overridden by the environment variable `TROVATO_RATE_LIMIT_<BUCKET>`
or by the site configuration key `rate_limit.<bucket>`, both taking the count
alone. **The environment wins over the stored configuration**, so a limit can
always be changed without reaching into the database. Both are read once at
startup, so a change to the stored value takes effect on the next restart. A
value that is not an integer of at least 1 is ignored with a warning and the
compiled default stands.

| Bucket | Default | Applies to |
|--------|---------|------------|
| `static` | 2000 / minute | Reads under `/static/` and `FILES_URL` (default `/files/`), and `/favicon.ico` |
| `page` | 600 / minute | Page views: a `GET` or `HEAD` outside `/api/` that no bucket above claims |
| `api` | 100 / minute | `/api/` endpoints, and any other request no bucket claims |
| `forms` | 30 / minute | Any other POST |
| `search` | 20 / minute | `/search` and `/api/v1/search` |
| `search_expand` | 30 / minute | `POST /api/v1/search/expand` |
| `search_summarize` | 10 / minute | `POST /api/v1/search/summarize` |
| `search_followup` | 5 / minute | `POST /api/v1/search/followup` |
| `login` | 5 / minute | `POST /user/login` |
| `register` | 3 / hour | `POST /user/register` |
| `uploads` | 10 / minute | `/file/upload` and `POST /api/v1/block-editor/upload` |
| `verify_email` | 10 / minute | Email verification token attempts |
| `profile` | 10 / minute | Profile update submissions |
| `password` | 5 / minute | Password change submissions |
| `recovery` | 5 / 15 minutes | Account recovery, per IP and per account |
| `comment` | 4 / minute | Comment writes |
| `data_export` | 1 / hour | Personal-data export downloads |
| `mail` | 100 / hour | Mail a plugin sends through the `mail` host interface. Keyed **per plugin**, not per client, and checked on every path — a request, `tap_cron`, `tap_queue_worker`, `tap_install` — because the mailbox being protected is the site's own. Also bounds `POST /admin/config/site/test-email` per client, which mails the same address. |

Every kernel route that writes a file, sends mail or calls an AI provider has
a named bucket, listed with it in the `SENSITIVE_ROUTES` test in
`crates/kernel/src/middleware/rate_limit.rs`. Some are checked by the route
itself rather than by path: password reset (`recovery` per client and
`reset_mail` per address), account recovery, profile updates, the data export,
and the AI chat and assistant, whose hourly limits are set on their admin
pages. `POST /api/v1/ai/assist` is still counted in `api` and bounded by the AI
token budget.

For example, to serve a site whose pages carry an unusual number of assets:

```bash
TROVATO_RATE_LIMIT_STATIC=5000
```

#### Counts by role

A bucket can give the holders of a role their own count, set by the environment
variable `TROVATO_RATE_LIMIT_<BUCKET>__<ROLE>` (two underscores) or the site
configuration key `rate_limit.<bucket>.<role>`, with the same precedence and the
same handling of a bad value as the bucket default. `<role>` is the role's name,
lower-cased, with each run of characters other than a letter or a digit written
as one underscore: `anonymous user` is `anonymous_user`, `Site Editor` is
`site_editor` (upper-cased in the environment variable). A key naming a role
nobody holds has no effect.

Which count a request gets:

- **No credential** (no session, no `Authorization: Bearer`): the
  `anonymous_user` count, or the bucket default, per client IP.
- **Signed in**, by cookie or by token: the most generous count among the user's
  roles, every signed-in user holding `authenticated_user` as well as their own,
  or the bucket default if none of them has one, per user. No role is exempt,
  administrators included.

Before authentication has run, a request that presents a credential is also
counted per client IP at the most generous count the bucket gives anyone, in a
budget of its own. That bounds an address sending tokens that authenticate
nobody without refusing a signed-in user their own count.

For example, 60 API requests a minute for anonymous clients and 300 for any
signed-in user:

```bash
TROVATO_RATE_LIMIT_API__ANONYMOUS_USER=60
TROVATO_RATE_LIMIT_API__AUTHENTICATED_USER=300
```

or, stored in the site configuration:

```text
rate_limit.api.anonymous_user = 60
rate_limit.api.authenticated_user = 300
```

`login` and `register` are counted once per request, by the per-IP check, so
`login` admits five attempts a minute from an address and `register` three an
hour.

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

## Update checks

Trovato asks GitHub, at most once a day from cron, whether a newer release exists.

**Exactly what that is:** one outbound HTTPS GET to
`https://api.github.com/repos/jeremyandrews/trovato/releases/latest`, carrying no
site data — no URL, no version, no site identifier, nothing but the request and a
`Trovato/<version>` User-Agent. GitHub learns that an IP address asked about a
public repository. Nothing is sent about the site, and no page render ever makes the
request: the check runs in cron and stores its answer, and the dashboard reads what
was stored.

When the running version is behind, **an administrator** sees a banner on
`/admin`, styled as an alarm when the release announces itself as a security
release (see the release convention in CONTRIBUTING.md). Nobody else sees it: a
visitor and a logged-in non-administrator are never told the site's version, which
is a fingerprinting detail with no upside for them.

**Turning it off.** There are two switches and the environment wins:

- `UPDATE_CHECK=0` in the environment, for a deployment where "no outbound
  requests" is an operational requirement. An air-gapped install should not depend
  on a database row being right.
- A **Check for Trovato releases** checkbox at `/admin/config/site`, for a site that
  simply does not want it.

On by default, which is a deliberate choice rather than an oversight: a site with no
way to learn that a security fix exists is a site that does not get it. Drupal core
has shipped update status on by default for two decades. The data flow is small
enough to write down in full, which is what the paragraph above is.

| Plugin | Description |
|--------|-------------|
| `trovato_blog` | Blog content type with tags |
| `trovato_categories` | Hierarchical taxonomy with tags |
| `trovato_comments` | Threaded comments on content |
| `trovato_media` | Media library and file management |
| `trovato_redirects` | URL redirect management |
| `trovato_audit_log` | Administrative audit trail |
| `trovato_scheduled_publishing` | Publish/unpublish content on a schedule |
| `trovato_content_locking` | Pessimistic content editing locks |
| `trovato_webhooks` | Outgoing webhook notifications |
| `trovato_image_styles` | Server-side image derivative generation |
| `trovato_oauth2` | OAuth2 authorization server (requires `JWT_SECRET`) |
| `trovato_locale` | Interface translation |
| `trovato_content_translation` | Translatable content fields |
| `trovato_config_translation` | Translatable configuration |
| `trovato_book` | Ordered page trees with previous/next/up navigation |

### Specialized Plugins (separate install)

The `plugins-disabled/` directory may contain plugins that require
additional infrastructure (e.g., external APIs or dedicated databases).
These are not loaded by default:

- `goose` &mdash; Load testing integration

Argus, the news-intelligence plugin (RSS and Atom ingest, relevance scoring,
story clustering), is not in this tree: it builds and installs from its own
repository, `jeremyandrews/argus`, as an overlay on a released kernel.

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

## More Than One Server

Several servers can run one site behind a load balancer, with no sticky
sessions and no extra setting, if they share one Postgres, one Redis and one
read-write volume for `UPLOADS_DIR` and `PAGEFIND_OUTPUT_DIR`. See
[docs/deployment/multi-server.md](docs/deployment/multi-server.md).

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
ls plugins/trovato_blog/blog.wasm          # should exist
ls plugins/trovato_blog/blog.info.toml     # should exist
```

**OAuth2 not working:**
The OAuth2 plugin requires `JWT_SECRET` to be set in your `.env` with a
value of at least 32 bytes. Without it, the plugin silently skips
initialization.

**Template rendering errors:**
Ensure `TEMPLATES_DIR` points to the `templates/` directory at the
project root. If running from a different working directory, set the
absolute path.
