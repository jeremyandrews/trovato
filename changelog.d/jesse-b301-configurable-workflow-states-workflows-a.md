Content moderation states and transitions were compiled into the kernel (Draft,
Published, Archived, and five fixed transitions) and enforced by a database
check, so a site could not add a review step without a kernel change. States,
transitions and permissions are now read from a `moderation.workflows` site
configuration variable, one workflow per moderated type, with the shipped
default equal to the old built-in Editorial workflow under the same permission
names, so an existing site sees no change.
