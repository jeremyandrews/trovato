-- no-transaction
-- The private-download route and `trovato files migrate` look up managed
-- files by visibility; the general listing/admin queries already use the
-- primary key or owner_id/status indexes.
--
-- CONCURRENTLY, alone in this no-transaction file, so building it does not
-- block writes to file_managed for the build's duration.
CREATE INDEX CONCURRENTLY idx_file_visibility ON file_managed (visibility) WHERE visibility = 1;
