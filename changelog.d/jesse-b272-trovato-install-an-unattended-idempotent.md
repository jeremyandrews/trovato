The first administrator could only ever be created by the web installer: a
fresh database redirected every path except `/install`, `/health` and
`/static` to it, `POST /cron/{key}` included, so a container stack with
nobody to open a browser at it never finished installing and cron never ran.

`trovato install --admin-user <user> --admin-mail <address>
--admin-password-stdin --site-name <name> [--site-mail <address>]` creates
the first administrator and completes the site step from the command line,
through the same service code the web installer's two POST handlers now
share. The password is read from stdin, or from a file named by
`TROVATO_INSTALL_ADMIN_PASSWORD_FILE`, never a command line argument. Every
value also has an environment variable (`TROVATO_INSTALL_ADMIN_USER`,
`TROVATO_INSTALL_ADMIN_MAIL`, `TROVATO_INSTALL_SITE_NAME`,
`TROVATO_INSTALL_SITE_MAIL`), so a container can carry the whole command in
its environment and run it before `serve` on every start: it is idempotent,
exiting 0 and changing nothing once the site is installed, and completes
only the missing step on a site with an administrator but no site step done
yet.
