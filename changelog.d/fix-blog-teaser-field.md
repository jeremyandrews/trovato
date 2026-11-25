A blog teaser in the blog listing has text in it.

`templates/gather/query--blog_listing.html` guarded its teaser on
`row.fields.body`. `body` is the field the kernel `page` type uses;
`trovato_blog` defines a blog post's long text as `field_body`
(`tap_item_info`), so the guard was never true and every post in the listing
rendered as a title, a date and a "Read more" link with nothing between them.
The template was written against the wrong content model, and because the miss
was a silent `is defined` guard rather than an error, it rendered a plausible
page instead of failing.

The template reads `field_body`. Neither field is renamed: that is a content
model change for existing sites and needs a migration and a decision.
`blog_listing_test` seeds a published post through the blog's own gather query
and requires the listing to show its body text.
