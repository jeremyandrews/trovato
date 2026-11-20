A design note, `docs/design/forms-pipeline.md`, fixes the shape of one kernel
forms pipeline before the code moves. The form taps were unreachable because no
route ever called `FormService::build` or `process`, and content forms came from
two separate renderers that each wrote HTML of their own, so a plugin's
alteration, a field's cardinality and the save that kept unshown fields had no
single place to live.
