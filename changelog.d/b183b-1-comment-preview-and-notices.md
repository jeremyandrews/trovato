The comment form has a Preview button, under the item and on the reply page.
It shows the comment above the form, still holding what was typed, as it will
read once posted: the body through the text format a posted comment is stored
in (`filtered_html`), in the comment pages' own markup (now shared through
`elements/comment-single.html`), and saves nothing. The form had one button,
so a commenter could not see the filtered result before it was public, which
Drupal's comment form always offered. With JavaScript on, `comment-post.js`
leaves the Preview button's submission to the browser.

A site can stop mailing content authors about new comments. Both notice
paths, a comment posted straight to published and a held comment approved
on the moderation screen, mailed the author whenever mail was configured,
with nothing to turn it off, so a site imported from one that never mailed
its authors would start mailing every real address it holds. "E-mail content
authors about new comments" now sits beside "New comments" in the comment
settings (`comment_notify_authors`). It is on unless it is unticked, so a
site that does nothing keeps today's behaviour. Both paths read it through
`comment_mail_context`, which they already shared, and the post path does one
more `site_config` read for it.
