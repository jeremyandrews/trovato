Search's result count and its pager could promise more than the page ever
showed, because the total came from a coarse SQL filter and the page was
checked against real item access afterward, so a row the count included could
still be denied once the page was built.

The total, the page count and the pager now come from the same rule the page
itself uses (`ItemService::search_visibility`), decided once per request
before the query runs, so they no longer drift apart by stage, publish status
or a missing permission. Any item on an internal-visibility stage, published
or not, or one only a plugin's own access check would allow, is left out of
search for every role but the site administrator. An author's own unpublished
item is additionally a candidate only when they hold `view own unpublished
content`, and even then not if it sits on an internal-visibility stage.
Searching also now asks for `access content` as well as `search content`,
refused the same way whichever is missing.
