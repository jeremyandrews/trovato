A gather row read in a translated language now carries the translation's
field values — body, summary, image alt text, every `fields.*` key — falling
back to the original item only for a key the translation does not set. The
title was already translated, because it lives in its own column; every other
field came from `item.fields`, merged with the translation as
`COALESCE(_tr.fields || item.fields, item.fields)`. PostgreSQL's `||` on JSONB
lets the *right* operand win on a duplicate key, and `item.fields` was the
right operand, so the original's keys overrode the translation's rather than
the other way around: a Spanish listing page showed an English body and an
English image alt text under a Spanish title.

The merge now puts the translation on the right (`item.fields || _tr.fields`),
and the same `COALESCE`-overlay rule is applied to the gather query builder's
other SQL projection, an explicit `fields.x` select, which previously read
straight from the base item and showed no translation at all regardless of
viewer language. The item page's own overlay (`routes/helpers.rs`) already
applied this rule correctly and needed no change; it's the reference the SQL
side was brought into line with.
