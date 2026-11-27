A benchmark of Gather's field queries at scale, `benchmarks/field-queries`
(`field-bench`), and its baseline in `docs/performance/field-queries.md`. It
seeds a dedicated database (never the shared test database) with an item type
of 200 fields of every kind and a million items of it, plus a million items of
four other types, with revisions, and runs 24 Gather definitions through the
real Gather service, cold and warm, recording latency percentiles, statement
counts, plans and buffers, save latency, sizes and the hot set. A run that
times out is counted, not dropped: a percentile that falls on a failed run
reads "timed out", and a before and after comparison refuses two different
fixtures. The saves it times are written back afterwards, so one seeded
database serves any number of runs.

The baseline shows the cost of field conditions written as expressions no
index covers. Items keep their fields in one JSONB column whose only index is
a GIN over the whole column, which serves whole column containment and nothing
Gather writes: a filter, sort or facet on a field reads every item of the type
and decompresses each one's whole field document to read one key. At a million
items most field queries take 0.2 to 16 seconds, several hit the ten second
Gather statement timeout, and a numeric range on an integer field
(`fields.price >= 5000`) fails outright with `operator does not exist: text >=
integer`, because the field is read as text and compared with a number.
