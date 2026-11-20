A plugin's `send-to-site-contacts` now sends from `tap_cron`, `tap_queue_worker`
and `tap_install`. **Behaviour change for plugin authors:** an existing host
function goes from always failing in the background to sending, bounded by the
per-plugin `mail` bucket that already applied on those paths (BL-72,
G-MAIL-UNAVAILABLE-IN-BACKGROUND).

The root cause was that background dispatch was built without the email
service. `RequestServices::for_background` leaves it unset, only the request
template attached it, and neither `CronService::background_services` (used for
`tap_cron`) nor the queue worker's own builder nor the `tap_install` dispatch
called `with_email`, although `CronService` already held the very same
`EmailService` for its `email:send` queue. So on a site with SMTP configured,
every plugin mail from cron or a queue worker returned `ERR_MAIL_NOT_CONFIGURED`
and logged "no SMTP host configured". `CronService` now keeps the service it is
given and attaches it in `background_services`, which the queue worker now uses
too instead of assembling the same services itself, and `tap_install` attaches
it beside the rate limiter. It is the one shared service, with the kernel's
SMTP transport, `from` address and circuit breaker.
