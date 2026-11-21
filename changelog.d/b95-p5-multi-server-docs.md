New `docs/deployment/multi-server.md`: what several servers share (one
Postgres, one Redis, one read-write volume such as a `ReadWriteMany` PVC, NFS
or EFS, for uploads and the Pagefind index), how a write reaches every server
within about 250 ms, why the AI circuit breakers stay per server, the rolling
restart that applies a plugin enable or disable, and how to run the two server
harness locally. The Pagefind swap has been tested on a Docker volume only.
`INSTALL.md` points to it, and `UPGRADING.md` lists what changed for every site.
The cluster compose file's test command now runs the scenarios one at a time,
as they require.

Two more tests that count a warm page's queries (`gather_tile_test`,
`menu_tree_test`) now let the cache generations their own writes bumped be
seen before they warm and count, as the revision page test already did; a
clear landing between the warm-up and the count made the count one higher.
