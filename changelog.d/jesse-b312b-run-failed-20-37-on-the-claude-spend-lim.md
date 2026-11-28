The SSRF fence that stops a plugin's outbound HTTP from reaching a private or
loopback address had no operator override, so a plugin needing a self-hosted
service on the same host or LAN — a Varnish cache a purge plugin must send
invalidations to, chief among them — could never reach it, however trusted
the operator's own deployment made that destination. `PLUGIN_HTTP_PRIVATE_ALLOW`
now lets an operator name one plugin, one destination and one port that
plugin may reach; every other plugin, port and destination still meets the
same refusal as before, and a redirect away from the granted destination is
refused the same way a redirect anywhere else private always is. Empty by
default, which is today's behaviour.
