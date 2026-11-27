Dead plugin-queue jobs, before reachable only through the JSON dead-letter
API or `psql`, now have an administration page: `/admin/reports/dead-jobs`,
under Reports, gated on `administer site` like the API it shares. Each row
shows the queue, the plugin, the job id, when it died, the reason and its
attempts, filterable by queue and by plugin, sorted, fifty a page. Retry
resets a job to `ready` with a fresh attempt budget; Discard removes it for
good and asks first, through the same confirmation page every other delete
uses. Both post to the same functions the JSON API calls, so there is one
query path for listing, requeuing and deleting a dead job, not two. The
Dashboard's Scheduled tasks panel now links straight to the page, in place
of the sentence naming the API endpoint, for whoever may open it.

"Who can do this?" named only a count under each role before. It now lists
up to twenty of a role's members, each linked to their account, then "and N
more" linking to the People listing filtered to that role (a filter the
People listing gained for this). Names are shown only to a viewer who may
open the People listing themselves; everyone else keeps the count, as
before. Every role's members and names come from one query, not one per
role, so the page's cost does not grow with the number of roles.
