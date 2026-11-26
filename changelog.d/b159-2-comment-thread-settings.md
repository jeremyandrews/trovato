Comments are set per content type, as in Drupal: open, closed (the thread is
shown and no new comment is taken) or hidden (no thread, no form); threaded or
flat; oldest or newest first; and how many comments a page of the thread
shows, 50 unless the type says otherwise. The content type form has a
"Comment settings" fieldset for the four (`comment_mode`,
`comment_threading`, `comment_order`, `comment_per_page` in the type's
settings). A type that says nothing is open, threaded, oldest first, 50 a
page, which is what every type did before apart from the paging.

Before, the item page rendered every published comment of the item, under
every item of every type, with a form: a 174 comment discussion was one page,
and a type whose comments the original site hid (a basic page, a gallery
image, a profile) still had a comment area. `render_thread` now reads the
type's settings, renders nothing for a hidden type and no form for a closed
one, and shows one page of the thread at `?page=N` (1-based like the kernel's
other pagers; a page past the end is the last page, a number that is not one
is the first) with the standard pager. The comment route refuses a comment on
an item whose type is not open, so closing a type is not only a matter of the
form not being offered. After a comment is posted or edited, the redirect
lands on the page of the thread that holds it.

Newest first orders the threads and leaves each reply under the comment it
answers, in the order replies were written. Drupal 6 sorted its thread path
descending for this, which put replies above their parents; that is not
copied. The thread query's sort key also breaks a tie on the second by id, so
comments written in the same second keep a stable order.

The item page makes one more query than before, the type's settings, whatever
the number of comments: 11 statements before and 12 after for an item with 50
comments, warm caches. Author names are looked up for the page shown rather
than the whole thread.

The comment pages added in the previous change take their heading as the page
title, and the stock theme lays out a comment's Reply, Edit and Delete links
as one row of small links rather than a bulleted list.
