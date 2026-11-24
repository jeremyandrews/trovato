The gap analysis skipped an actions/trigger system, reasoning that the tap
system plus webhooks already cover the same ground. That held for a plugin
author, and said nothing about trovato.blog's blog owners, who are not
developers and cannot write a tap. `docs/design/rules.md` reverses the
decision for that audience and designs an events/conditions/actions layer
with no expression language and no code action: an additive kernel event
catalogue every tap already dispatches into automatically, a fixed condition
and action set that calls the same kernel services and permission checks as
any other write, and a per-blog, least-privilege service account that can
never hold more than its owner granted it. `docs/alignment/gap-analysis.md`
and `docs/alignment/roadmap.md` record the reversal, and `docs/BACKLOG.md`
gets ten new rows (BL-154 to BL-163) for the build parts that follow, in
dependency order. Nothing in this change is built yet; it is the design the
build parts will implement against.
