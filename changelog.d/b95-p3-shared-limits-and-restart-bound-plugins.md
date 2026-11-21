The chat, assistant and AI provider rate limits now count in Redis, keyed as
before (per user or IP per hour for chat and the assistant, per provider per
minute for AI requests), so a site on several servers refuses at the limit
rather than at the limit times the number of servers. They fail open when Redis
cannot be reached, like the other Redis limits. The AI circuit breakers stay
per server on purpose.

A plugin's `tap_install` is claimed in `plugin_status` before it is dispatched,
so when several servers boot together against a database with a plugin
pending, exactly one of them runs it. As before, a dispatch that fails stays
marked; reset `tap_install_called` to run it again.

Enabling or disabling a plugin on the admin page now changes only its stored
status, as the CLI already did. The running server keeps the plugins it
started with, so its routes, taps and menus no longer disagree with the other
servers'. The page says that changes take effect when the server restarts (all
servers, if more than one) and marks each plugin whose stored status waits for
a restart. Dependencies are checked against the stored statuses, and a newly
enabled plugin's permissions register at that restart, as they did whenever its
module was not yet loaded.
