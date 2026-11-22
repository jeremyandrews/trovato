Text formats are stored configuration with an admin screen. The three formats
(`plain_text`, `filtered_html`, `full_html`) were defined in code, so a site
could not change which tags `filtered_html` keeps or which roles may write in
it. They are rows of a new `text_format` table now, seeded by a migration with
exactly the code's definitions; a test renders the same inputs through the code
pipelines and through the stored rows and compares them, so an existing site
renders the same after upgrading as before.

`/admin/config/formats` lists the formats and edits each one's name, order,
allowed tags and attributes, line break and link conversion, and which roles may
use it; it adds formats and deletes any but the three built-in ones, whose
filter is also fixed. It takes the new `administer text formats` permission,
which no migration grants. A format's roles are its `use <format>` permission,
the same grant the permission grid shows (a site's own formats appear there
too), and a non-superuser may change it only for a format they may use
themselves. `<script>`, `<style>`, event handler attributes and `style` can
never be allowed.

Rendering reads the stored formats: the kernel's field rendering, the
`text-format.filter-text` host function and the item forms' format selector all
go through one in-process registry, loaded at start, reloaded when a format is
saved, and reloaded on every other server through a new `text_formats` cache
generation. A field offers only the formats the editor's roles may use, and a
save with any other is refused, as before. A format that does no filtering is
still never rendered from stored content: it shows as plain text, as `full_html`
always has.

Formats are a config entity, `text_format.<name>.yml`, exported and imported
before roles so a role file may grant a format the same set creates. An imported
format is checked by the same rules as the form.
