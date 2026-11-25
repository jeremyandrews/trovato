#!/usr/bin/env bash
# Prepare a site for the admin accessibility checks: migrate DATABASE_URL,
# install the plugins whose admin pages the checks cover, and write a small
# generated site (posts, comments, tags, members, and gen_admin).
#
#   TROVATO=target/debug/trovato ADMIN_A11Y_PASSWORD=… scripts/seed-site.sh
#
# Run from the repository root, against an empty database: it is what the CI
# job "Admin accessibility" runs, and the same on a laptop.
set -euo pipefail

TROVATO="${TROVATO:-target/debug/trovato}"
: "${DATABASE_URL:?set DATABASE_URL}"
: "${ADMIN_A11Y_PASSWORD:?set ADMIN_A11Y_PASSWORD}"

# The plugins with admin pages of their own, and those generate writes for.
# The browser checks read the same list: a 404 from one of these plugins'
# pages is a failure, not a plugin that is off.
read -r -d "" -a PLUGINS < "$(dirname "$0")/seed-plugins.txt" || true

"$TROVATO" migrate
for plugin in "${PLUGINS[@]}"; do
    "$TROVATO" plugin install "$plugin"
done
"$TROVATO" migrate
"$TROVATO" generate --items 40 --comments 60 --tags 24 --users 30 \
    --admin-password "$ADMIN_A11Y_PASSWORD"
