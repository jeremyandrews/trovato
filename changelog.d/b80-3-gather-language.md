Gather can express `or` now, and it can summarise rows. `filter_groups`
holds parenthesised groups, ANDed with `filters`. Each group joins its filters
and nested groups with `and` or `or`, up to three deep. Grouped filters get
the same resolution as top-level ones: category hierarchies, custom operators,
contextual values and `url`. Exposed and `semantic_similarity` filters stay
top-level.

`group_by`, `aggregates` (`count`, `sum`, `min`, `max`, `avg`, each labelled),
`having` (a label compared with a number) and `distinct` summarise the rows of
a base that is not content, which in practice is a plugin's own table through
the `query` interface. The total counts groups. On `item` and on record types
they are refused. There, rows and fields are decided one at a time after the
query, so an aggregate would count or sum what the viewer may not see;
counting content per group stays open as BL-30.

Through `query`, grouped filters and `having` count toward the 32-filter bound.
Nesting past three, or more than 8 `group_by` columns or aggregates, returns
`ERR_QUERY_BOUND` (-81).

The admin gather builder has no control for these shapes. It used to rebuild
a definition from its controls on save, so it would have dropped them. It now
refuses to save a gather that has them, and its edit form says so and turns
saving off.

Contract: the definition keys are **additive**. A definition without them
emits the same SQL, which the golden tests check. `QueryDefinition` gains two
public fields, so a Rust struct literal of it in kernel-linked code needs them
or `..Default::default()` (see `UPGRADING.md`). No WIT, SDK or host function
signature changes.
