-- Add purpose column to distinguish registration vs email-change tokens.
ALTER TABLE email_verification_tokens
    ADD COLUMN purpose VARCHAR(20) NOT NULL DEFAULT 'registration';
