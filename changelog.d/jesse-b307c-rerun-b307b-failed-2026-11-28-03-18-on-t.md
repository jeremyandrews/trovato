Server search matches a typed word as a prefix again (`kernel` finds
`kerneltrap` and `kernels`), the way it did before B160.

B160 moved the query to `websearch_to_tsquery` so that punctuation in a
search (`noank & mystic`, a stray quote, an apostrophe) could never be read
as tsquery syntax and answer a 500. `websearch_to_tsquery` has no prefix
operator, so prefix matching was lost as a side effect of that fix, not as a
decision. The query is now built by hand again, with every word quoted as a
tsquery literal so none of it is ever syntax, and a bare word marked a
prefix; a quoted phrase still matches only exactly, `-word` still excludes,
and `a or b` still matches either.
