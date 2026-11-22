A site can switch its theme from `/admin/appearance`. Until now the only way to
change how a site looked was to replace template files and restart.

A theme is a directory under the new `THEMES_DIR` search path (default
`./themes`, split like `TEMPLATES_DIR`, a later entry's theme replacing an
earlier one of the same name) holding a `theme.toml` with a name, a description
and an optional screenshot, a `templates/` directory, and a `static/` directory
served at `/static/themes/<name>/`. The stock theme is the template search path
the kernel has always used. Each theme is loaded at start as the stock search
path with the theme's templates laid over it, so a template the theme does not
provide is the stock one; a theme whose templates fail to load, or whose
manifest is missing, is skipped with a warning rather than stopping the server.

`/admin/appearance` lists the themes with their screenshots and sets the default
theme, which draws every page outside `/admin`, and the administration theme,
which draws `/admin`. Both default to the stock theme, so an existing site looks
exactly as before. The choice is stored in `site_config` (`theme_default` and
`theme_admin`), read once at start, applied by the template loader as soon as
it is saved, and on every other server through a new `themes` cache generation:
no restart. A stored theme that is no longer installed is the stock theme. The
screen goes through the forms pipeline and takes the new `administer themes`
permission, which no migration grants.
