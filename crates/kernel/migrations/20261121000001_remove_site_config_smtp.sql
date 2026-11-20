-- SMTP configuration has one source, the environment (B26).
--
-- `/admin/config/site` used to write these six keys, the password in the clear.
-- Nothing ever read them: mail is sent with `SMTP_HOST`, `SMTP_PORT`,
-- `SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_ENCRYPTION` and `SMTP_FROM_EMAIL`
-- from the environment. Deleting them removes a stored credential that did
-- nothing but sit in the database and in every config export. UPGRADING.md
-- gives the query to read them back before upgrading.
DELETE FROM site_config
WHERE key IN (
    'smtp_host',
    'smtp_port',
    'smtp_username',
    'smtp_password',
    'smtp_encryption',
    'smtp_from'
);
