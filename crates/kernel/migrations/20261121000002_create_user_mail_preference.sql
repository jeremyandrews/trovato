-- Per user, per plugin mail preference for `mail.send-to-user` (BL-94).
--
-- A row is created on the first send to a user from a plugin. `token` goes in
-- that mail's unsubscribe link and `List-Unsubscribe` header: 32 random bytes,
-- URL safe base64. It authorizes one thing, opting this user out of this
-- plugin's mail, so it needs no secret key and no expiry. `opted_out_at` set
-- means the plugin may not mail this user; clearing it resubscribes.
CREATE TABLE user_mail_preference (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plugin text NOT NULL,
    token text NOT NULL UNIQUE,
    opted_out_at timestamptz NULL,
    created timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, plugin)
);
