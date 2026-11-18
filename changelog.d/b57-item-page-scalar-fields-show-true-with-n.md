Plain string, number and boolean fields on an item page show a label again,
and it is the one the item type declares. The item view wrote them as
`<strong class="field__label">label</strong>: value`, and the theme hid every
`.field__label`, although its comment said the rule was for body fields. The
label went and the `: ` stayed, so a Netgrasp person Item read `: true` and
`: false` with nothing to say what either was. The separator now sits inside
the label element, so a hidden label takes it along; the theme hides labels
only on `field_body` and `field_content`; and the label is read from the type's
field definition, falling back to the machine name (`field_notify_arrive` as
"notify arrive") only when the type declares none.

The Cancel button on a comment reply works, and the comment block raises no
Content Security Policy error. It called `cancelReply()` from an inline
`onclick`, which the kernel's `script-src` (no `'unsafe-inline'`) blocks, so the
click logged a violation and did nothing. `comment-reply.js` binds the button
itself now, and the `window.cancelReply` global that only served the inline
handler is gone.
