A site can take the contact form away from some roles or all of them.
`trovato_contact` declared both `/contact` routes with an empty permission,
which the kernel reads as "anybody", so there was nothing to revoke. Drupal
had `access site-wide contact form` for this (jibnotes denied it to
everyone). The plugin now declares that permission and both routes ask for
it: without it, a signed-in role gets 403, and a visitor who is not signed in
gets 401, the kernel's answer on every plugin route. A plugin migration
grants it once to the anonymous and authenticated roles, when the plugin is
first enabled or when a site that already has it enabled upgrades, so a site
that does nothing keeps a form anybody can use. A role it is later revoked
from stays revoked.
