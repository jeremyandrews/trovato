//! Accounts, with the ids and dates the file gives.
//!
//! Checked as the admin "Add user" form and `trovato user create` check an
//! account (`routes::admin_user::new_account_problems`): a name is required,
//! an email address is not, and whichever of them is given may not belong to
//! another account, ignoring case. A password arrives only as a hash, and
//! only as an Argon2 PHC string, the one format
//! `User::verify_password` accepts; anything else is refused, never stored,
//! and never echoed. No `tap_user_register`: a migrated account gets no
//! welcome mail. Roles are granted by name; none is taken away, and the
//! superuser flag is not something a file can set.
//!
//! Not through `UserService` (no tap, the file's id and date, one
//! transaction), but with the same security events it writes, built by the
//! same functions: `account.created` (path `import`), `user.name_changed`,
//! `user.mail_changed`, `user.status_changed`, `password.set` or
//! `password.changed` (via `import`), and `user.roles_changed` for each role
//! granted. They are queued on the [`Run`] and written once the
//! import commits, so a dry run or a run rolled back records nothing.

use anyhow::{Context, Result};
use uuid::Uuid;

use super::record::UserRecord;
use super::{Outcome, Run, require_id};
use crate::services::user::{
    AccountFields, account_change_events, account_created_event, password_event,
};

/// The column limit of `users.name` and `users.mail`.
const MAX_LENGTH: usize = 255;

/// Whether `hash` is an Argon2 PHC string the kernel can verify a password
/// against.
fn is_verifiable_hash(hash: &str) -> bool {
    argon2::PasswordHash::new(hash).is_ok_and(|h| {
        matches!(h.algorithm.as_str(), "argon2id" | "argon2i" | "argon2d") && h.hash.is_some()
    })
}

/// A stored account, as a record would describe it.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredUser {
    name: String,
    mail: String,
    status: i16,
    created: i64,
    pass: String,
}

impl StoredUser {
    /// The fields whose change is a security event. A file cannot set the
    /// administrator flag, so an import never changes it.
    fn fields(&self) -> AccountFields<'_> {
        AccountFields {
            name: &self.name,
            mail: &self.mail,
            status: self.status,
            is_admin: false,
        }
    }
}

/// Import one `user` record.
pub(crate) async fn import_user(run: &mut Run<'_>, r: UserRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    let name = r.name.trim().to_string();
    let mail = r.mail.trim().to_string();
    if name.is_empty() {
        anyhow::bail!("Username is required.");
    }
    if name.chars().count() > MAX_LENGTH || mail.chars().count() > MAX_LENGTH {
        anyhow::bail!("name and mail are at most {MAX_LENGTH} characters");
    }
    let status = r.status.unwrap_or(1);
    super::require_flag(status, "status")?;
    if let Some(hash) = &r.password_hash
        && !is_verifiable_hash(hash)
    {
        // The value is not repeated: it is a credential.
        anyhow::bail!(
            "password_hash is not an Argon2 PHC string ($argon2id$...), the only format \
             Trovato verifies; leave it out, and set a password with \
             `trovato user reset-password` after the import"
        );
    }

    let stored: Option<StoredUser> = sqlx::query_as(
        "SELECT name, mail, status, extract(epoch FROM created)::bigint AS created, pass \
         FROM users WHERE id = $1 FOR UPDATE",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the user")?;

    let taken: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE lower(name) = lower($1) AND id <> $2")
            .bind(&name)
            .bind(r.id)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up the name")?;
    if taken.is_some() {
        anyhow::bail!("Username '{name}' is already taken.");
    }
    // Empty (no address) is never unique, so it is never looked up: every
    // such account would otherwise collide with every other one.
    let taken: Option<Uuid> = if mail.is_empty() {
        None
    } else {
        sqlx::query_scalar("SELECT id FROM users WHERE lower(mail) = lower($1) AND id <> $2")
            .bind(&mail)
            .bind(r.id)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up the email address")?
    };
    if taken.is_some() {
        anyhow::bail!("Email '{mail}' is already in use.");
    }

    let mut roles = Vec::new();
    for role in &r.roles {
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM roles WHERE name = $1")
            .bind(role)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up a role")?;
        match id {
            Some(id) => roles.push(id),
            None => anyhow::bail!("Role '{role}' not found."),
        }
    }

    // Without a hash in the file, a stored password stays: one set with
    // `reset-password` after the first run is not wiped by the next.
    let pass = match (&r.password_hash, &stored) {
        (Some(hash), _) => hash.clone(),
        (None, Some(stored)) => stored.pass.clone(),
        (None, None) => String::new(),
    };
    let wanted = StoredUser {
        name,
        mail,
        status,
        created: r.created,
        pass,
    };
    let missing_roles: Vec<Uuid> = {
        let held: Vec<Uuid> =
            sqlx::query_scalar("SELECT role_id FROM user_roles WHERE user_id = $1")
                .bind(r.id)
                .fetch_all(&mut *run.tx)
                .await
                .context("failed to read the user's roles")?;
        roles.into_iter().filter(|r| !held.contains(r)).collect()
    };
    if stored.as_ref() == Some(&wanted) && missing_roles.is_empty() {
        return Ok(Outcome::Unchanged);
    }

    if stored.is_some() {
        sqlx::query(
            "UPDATE users SET name = $2, mail = $3, status = $4, created = to_timestamp($5), \
             pass = $6 WHERE id = $1",
        )
        .bind(r.id)
        .bind(&wanted.name)
        .bind(&wanted.mail)
        .bind(wanted.status)
        .bind(wanted.created as f64)
        .bind(&wanted.pass)
        .execute(&mut *run.tx)
        .await
        .context("failed to update the user")?;
    } else {
        sqlx::query(
            "INSERT INTO users (id, name, pass, mail, is_admin, status, created) \
             VALUES ($1, $2, $3, $4, false, $5, to_timestamp($6))",
        )
        .bind(r.id)
        .bind(&wanted.name)
        .bind(&wanted.pass)
        .bind(&wanted.mail)
        .bind(wanted.status)
        .bind(wanted.created as f64)
        .execute(&mut *run.tx)
        .await
        .context("failed to insert the user")?;
    }
    // The security events every other path writes for the same changes,
    // built by the same functions, and queued for after the commit.
    match &stored {
        Some(stored) => {
            run.security_events.extend(account_change_events(
                r.id,
                None,
                &stored.fields(),
                &wanted.fields(),
            ));
            if stored.pass != wanted.pass {
                run.security_events.push(password_event(
                    r.id,
                    None,
                    !stored.pass.is_empty(),
                    "import",
                ));
            }
        }
        None => run.security_events.push(account_created_event(
            r.id,
            None,
            "import",
            wanted.status == 1,
        )),
    }
    for role in missing_roles {
        let granted = sqlx::query(
            "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(r.id)
        .bind(role)
        .execute(&mut *run.tx)
        .await
        .context("failed to grant a role")?;
        // Audited only when the insert added the row: `ON CONFLICT DO NOTHING`
        // adds none when the grant already exists (another writer made it
        // since the lookup above), and no event may claim a grant this import
        // did not make.
        if granted.rows_affected() > 0 {
            run.role_grants.push((r.id, role));
        }
    }
    Ok(if stored.is_some() {
        Outcome::Updated
    } else {
        Outcome::Created
    })
}
