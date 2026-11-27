Search counts only the results it shows. A search for "Kerneltrap" on a
migrated site read "11 results", "Showing 10" and offered "Show more results"
over an empty list, and a search for a short word such as "go" read "50
results" over nothing at all.

The root cause is Pagefind's match logic. A term with no indexed word starting
with it is answered with the longest indexed word the term itself starts with
(`find_word_extensions` in pagefind_web, "checking the inverse"), so
"kerneltrap", which no page contains, returned every page with "kernel",
"kernels" or "HttpKernel" on it. The index and the fragments agree: each
returned location is a "kernel" word in the page's own content (body or a
comment), so the export was not at fault. `scolta.js` already kept such a page
off the screen, since its false positive check finds the term in none of the
excerpt, content or title, but it counted the header, "Showing N" and "Show
more" from Pagefind's raw hits and skipped the cards only while rendering, so
every card on a page could be skipped under a count that included them. A query
whose words were all shorter than three characters had no term to check at
all, and the check rejected every hit.

The check now runs before anything is counted: false positives leave the result
set as soon as it is loaded, so the header, "Showing N", the site facet counts,
the logged result count and the AI summary all describe cards that render, a
page is ten rendered cards, and a search whose every hit is a false positive
shows "No results found" (and, for several words, tries the OR fallback). A
query of only short words checks for those words. Pagefind's fallback is in
its compiled search code and stays; the check is what keeps it off the page.
