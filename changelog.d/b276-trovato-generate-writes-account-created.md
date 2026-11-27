`trovato generate` created accounts, including an administrator, without a
security event. Every other way an account is made (registration, the admin
form, the installer, `trovato user create`, `trovato import`) records
`account.created`, so a generated site held a full-rights `gen_admin` that
nothing in the security log said existed.

Each generated account now gets its `account.created` event, with
`details.path` `generate` and `details.by` `operator`, and the administrator's
also carries `details.admin`. The events are written set based, one
`INSERT ... SELECT` per kind of account in the transaction that writes the
accounts, never one statement per account, so generation stays a bulk tool.
That still costs a little under one second per hundred thousand accounts,
mostly the foreign key check and index entries of each event row.
