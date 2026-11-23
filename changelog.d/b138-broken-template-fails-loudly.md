A template with a syntax error no longer turns the whole site into
untemplated output. Every template was loaded into one Tera instance with
`add_template_files`, so a parse error in any single file failed the whole
load; `AppState` then logged one warning ("failed to load templates, using
empty engine") and carried on with an engine that had no templates at all, and
the error itself said only "failed to load templates from the search path".

Each template on the search path is now parsed on its own before Tera loads
them together, so the error names every file that does not parse, by path, with
Tera's message and the line and column it points at. A kernel, plugin or app
template that does not parse stops startup: the error is logged once at
`error` and the process exits with it. The empty-engine fallback is gone;
`ThemeEngine::empty` remains for tests.

A deployment theme (`THEMES_DIR`) whose templates do not parse is refused
instead: logged once at `error`, so it is in the site log, and the site keeps
running on the stock theme. `/admin/appearance` lists it as a theme that
cannot be activated, with the file and Tera's message, and choosing it is
refused with that reason while the active theme stays as it was.

Template names, load order and override rules are unchanged, and the check runs
only when templates are built, never per request.
