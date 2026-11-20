SMTP is configured in the environment and nowhere else, and the site settings
page no longer stores an SMTP password. **Behaviour change:** `/admin/config/site`
has no SMTP inputs any more, a POST that carries them stores nothing, and
migration `20261121000001_remove_site_config_smtp` deletes the six `smtp_*` keys
from `site_config`.

The root cause was a form with no reader. The page wrote `smtp_host`,
`smtp_port`, `smtp_username`, `smtp_password`, `smtp_encryption` and
`smtp_from` into `site_config`, the password in the clear unless it began with
`env:`, but the one `EmailService` has only ever been built from `SMTP_*` in the
environment. So the values an administrator typed changed nothing about how
mail was sent, and every database where one was typed held a credential in
clear text, in backups and in every config export (which dumps `site_config`
whole). The fieldset is replaced by a read only summary of what the kernel is
actually using: host, port, encryption, from address, and whether a username
and a password are set, never their values. "Send test email" works as before.
See `UPGRADING.md` for recovering the stored values before the migration deletes
them.
