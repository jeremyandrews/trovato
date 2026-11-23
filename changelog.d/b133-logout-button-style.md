The Logout control in the stock theme's header now looks like the header links
beside it. Logout is a POST form with a CSRF token, so `templates/page.html`
renders it as a `<button>` carrying both `site-nav__link` and
`site-nav__logout`. The `.site-nav__logout` reset in `static/css/theme.css`
came later in the sheet than `.site-nav__link`, at the same specificity, and
declared `font: inherit`, `color: inherit` and `padding: 0`; those won over the
link style, so Logout took the list item's 16px regular text and darker colour
instead of the links' 14px medium grey, and lost their padding and hover pill.
The reset now takes away only what a browser gives a button and not a link
(border, background, margin, appearance, font family and line height) and sets
nothing the link style sets, so the button gets the links' size, weight,
colour, padding, hover and focus ring at every width. Logout is still a POST
with its CSRF token; the template is unchanged.
