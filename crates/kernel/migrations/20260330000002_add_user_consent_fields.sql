-- Add GDPR consent tracking fields to the users table.
-- These fields allow plugins to record when a user consented,
-- to which privacy policy version, and their retention preference.
-- All fields are nullable — consent is not required for basic registration.

ALTER TABLE users ADD COLUMN consent_given BOOLEAN DEFAULT FALSE;
ALTER TABLE users ADD COLUMN consent_date TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN consent_version VARCHAR(64);
ALTER TABLE users ADD COLUMN data_retention_days INTEGER;
