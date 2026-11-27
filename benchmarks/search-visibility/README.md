# Search visibility benchmark (B248)

Times `/search` for three viewers on a generated site and shows the query
plans of the search statements. Use it to compare two builds on the same
database.

1. Create an empty database whose name starts with `search_bench`.
2. Seed it from the repository root. `ITEMS` blog posts are written by
   `trovato generate`; 15 percent of them become unpublished and 5 percent
   move to an internal stage. `gen_user_1` gets `view own unpublished content`.

   ```sh
   DATABASE_URL=postgres://trovato:trovato@127.0.0.1:55248/search_bench \
   PSQL="psql postgres://trovato:trovato@127.0.0.1:55248/search_bench" \
   TROVATO=target/release/trovato ITEMS=60000 \
   benchmarks/search-visibility/seed.sh
   ```

3. Start a build against it, with the search and login rate limits raised so
   the benchmark is not refused:

   ```sh
   TROVATO_RATE_LIMIT_SEARCH=10000000 TROVATO_RATE_LIMIT_LOGIN=100000 \
   MIGRATE_ON_START=false PORT=3248 target/release/trovato serve
   ```

4. Time it. Each viewer (anonymous, `gen_user_1`, `gen_admin`) sends
   `--requests` searches after 20 warm-up requests:

   ```sh
   python3 -I benchmarks/search-visibility/measure.py \
     --base http://127.0.0.1:3248 --label branch --requests 400 --out branch.json
   ```

5. Plans: `psql -v author="'<gen_user_1 id>'" -f benchmarks/search-visibility/explain.sql`.

Stop the server, start the other build on the same database, and repeat steps
3 and 4. Alternate the builds over several rounds: one round on a shared
machine is noise.
