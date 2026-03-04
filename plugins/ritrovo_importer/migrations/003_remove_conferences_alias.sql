-- Remove any URL alias pointing at /conferences.
--
-- /conferences is now handled directly by the ritrovo_topics route and does
-- not need a url_alias entry.  The path_alias middleware runs before routing,
-- so an alias for /conferences would rewrite the URI and prevent the
-- ritrovo_topics handler from ever being reached.
--
-- This migration cleans up any stale alias that may have been created during
-- Part 1 of the tutorial (/conferences → /gather/upcoming_conferences) or by
-- an earlier version of tap_install.

DELETE FROM url_alias WHERE alias = '/conferences';
