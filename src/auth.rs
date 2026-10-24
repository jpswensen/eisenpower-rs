//! User accounts, password hashing, and signed session cookies.
//!
//! Accounts live in `<data_dir>/_auth/users.db` so they can be managed while the
//! server is running. `_auth` is a directory, and per-user task databases are
//! files named `<username>.db`, so the two can never collide.

use anyhow::Context;
use argon2::{
    password_hash::{PasswordHash, SaltString},
    Argon2, PasswordHasher, PasswordVerifier,
};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

pub const SESSION_COOKIE: &str = "eisenpower_session";

/// Signature scheme version, so a future change can invalidate old cookies.
const SESSION_VERSION: &str = "v1";

type HmacSha256 = Hmac<Sha256>;

/// A user as stored in the accounts database.
#[derive(Debug, Clone)]
pub struct User {
    pub username: String,
    pub is_admin: bool,
    pub created_at: String,
}

/// The identity behind an authenticated request.
#[derive(Debug, Clone)]
pub struct Session {
    pub username: String,
    pub is_admin: bool,
}

pub struct AuthConfig {
    pub data_dir: PathBuf,
    session_secret: Vec<u8>,
    session_max_age: i64,
    cookie_secure: bool,
    users: SqlitePool,
    login_attempts: Mutex<HashMap<String, (u32, i64)>>,
}

/// Failed logins allowed per username inside `LOGIN_WINDOW_SECS`.
const LOGIN_MAX_ATTEMPTS: u32 = 10;
const LOGIN_WINDOW_SECS: i64 = 300;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn is_valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Argon2id is deliberately slow and memory-hungry: the default parameters cost
/// roughly 19 MB per call. Capping how many run at once keeps a burst of
/// sign-in attempts from exhausting a small container's memory budget, and
/// `spawn_blocking` keeps that work off the async runtime so one sign-in does
/// not stall every other request on a slow CPU.
static HASH_SLOTS: Semaphore = Semaphore::const_new(2);

async fn hash_password(password: &str) -> anyhow::Result<String> {
    let _permit = HASH_SLOTS.acquire().await?;
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|err| anyhow::anyhow!("failed to hash password: {err}"))
    })
    .await?
}

async fn verify_password(password: &str, stored_hash: &str) -> bool {
    let Ok(_permit) = HASH_SLOTS.acquire().await else {
        return false;
    };
    let password = password.to_string();
    let stored_hash = stored_hash.to_string();
    tokio::task::spawn_blocking(move || {
        let Ok(parsed) = PasswordHash::new(&stored_hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
    .await
    .unwrap_or(false)
}

impl AuthConfig {
    pub async fn from_env() -> anyhow::Result<Self> {
        let data_dir = env::var("EISENPOWER_DATA_DIR")
            .or_else(|_| env::var("EISENHOWER_DATA_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));

        let auth_dir = data_dir.join("_auth");
        tokio::fs::create_dir_all(&auth_dir)
            .await
            .with_context(|| format!("creating auth directory {}", auth_dir.display()))?;

        let options = SqliteConnectOptions::new()
            .filename(auth_dir.join("users.db"))
            .create_if_missing(true);
        let users = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .context("opening the accounts database")?;
        init_users_schema(&users).await?;

        let session_secret = load_or_create_session_secret(&auth_dir).await?;

        let session_max_age = env::var("EISENPOWER_SESSION_MAX_AGE")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|secs| *secs > 0)
            .unwrap_or(60 * 60 * 24 * 30);

        let cookie_secure = env::var("EISENPOWER_COOKIE_SECURE")
            .map(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        let config = Self {
            data_dir,
            session_secret,
            session_max_age,
            cookie_secure,
            users,
            login_attempts: Mutex::new(HashMap::new()),
        };

        config.seed_users_from_env().await?;
        Ok(config)
    }

    pub fn user_db_path(&self, username: &str) -> PathBuf {
        self.data_dir.join(format!("{username}.db"))
    }

    // ---- account management -------------------------------------------------

    pub async fn list_users(&self) -> anyhow::Result<Vec<User>> {
        let rows = sqlx::query(
            "SELECT username, is_admin, created_at FROM users ORDER BY is_admin DESC, username ASC",
        )
        .fetch_all(&self.users)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| User {
                username: row.get("username"),
                is_admin: row.get::<i64, _>("is_admin") != 0,
                created_at: row.get("created_at"),
            })
            .collect())
    }

    pub async fn find_user(&self, username: &str) -> anyhow::Result<Option<User>> {
        let row =
            sqlx::query("SELECT username, is_admin, created_at FROM users WHERE username = ?1")
                .bind(username)
                .fetch_optional(&self.users)
                .await?;
        Ok(row.map(|row| User {
            username: row.get("username"),
            is_admin: row.get::<i64, _>("is_admin") != 0,
            created_at: row.get("created_at"),
        }))
    }

    pub async fn admin_count(&self) -> anyhow::Result<i64> {
        let (count,) = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM users WHERE is_admin = 1")
            .fetch_one(&self.users)
            .await?;
        Ok(count)
    }

    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        is_admin: bool,
    ) -> anyhow::Result<()> {
        if !is_valid_username(username) {
            anyhow::bail!("Usernames may only contain letters, numbers, '.', '-', or '_'.");
        }
        if password.len() < 8 {
            anyhow::bail!("Passwords must be at least 8 characters.");
        }
        if self.find_user(username).await?.is_some() {
            anyhow::bail!("A user named '{username}' already exists.");
        }
        let hash = hash_password(password).await?;
        sqlx::query("INSERT INTO users(username, password_hash, is_admin) VALUES (?1, ?2, ?3)")
            .bind(username)
            .bind(hash)
            .bind(if is_admin { 1_i64 } else { 0_i64 })
            .execute(&self.users)
            .await?;
        Ok(())
    }

    /// Changes a password and invalidates that user's existing session cookies.
    pub async fn set_password(&self, username: &str, password: &str) -> anyhow::Result<()> {
        if password.len() < 8 {
            anyhow::bail!("Passwords must be at least 8 characters.");
        }
        let hash = hash_password(password).await?;
        let result = sqlx::query(
            r#"
            UPDATE users
            SET password_hash = ?1,
                session_epoch = session_epoch + 1,
                updated_at = datetime('now')
            WHERE username = ?2
            "#,
        )
        .bind(hash)
        .bind(username)
        .execute(&self.users)
        .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!("No user named '{username}'.");
        }
        Ok(())
    }

    pub async fn set_admin(&self, username: &str, is_admin: bool) -> anyhow::Result<()> {
        let result = sqlx::query(
            "UPDATE users SET is_admin = ?1, updated_at = datetime('now') WHERE username = ?2",
        )
        .bind(if is_admin { 1_i64 } else { 0_i64 })
        .bind(username)
        .execute(&self.users)
        .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!("No user named '{username}'.");
        }
        Ok(())
    }

    pub async fn delete_user(&self, username: &str) -> anyhow::Result<()> {
        let result = sqlx::query("DELETE FROM users WHERE username = ?1")
            .bind(username)
            .execute(&self.users)
            .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!("No user named '{username}'.");
        }
        Ok(())
    }

    // ---- login --------------------------------------------------------------

    /// Returns the session for a valid `username`/`password` pair.
    ///
    /// `Err(LoginError::RateLimited)` is returned once a username has collected
    /// too many recent failures, so a weak password cannot simply be guessed.
    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Session, LoginError> {
        if self.is_rate_limited(username) {
            return Err(LoginError::RateLimited);
        }

        let row = sqlx::query("SELECT password_hash, is_admin FROM users WHERE username = ?1")
            .bind(username)
            .fetch_optional(&self.users)
            .await
            .map_err(|err| LoginError::Internal(err.into()))?;

        let Some(row) = row else {
            // Hash anyway so a missing user costs the same as a wrong password.
            let _ = hash_password(password).await;
            self.record_failure(username);
            return Err(LoginError::InvalidCredentials);
        };

        let stored_hash: String = row.get("password_hash");
        if !verify_password(password, &stored_hash).await {
            self.record_failure(username);
            return Err(LoginError::InvalidCredentials);
        }

        self.clear_failures(username);
        Ok(Session {
            username: username.to_string(),
            is_admin: row.get::<i64, _>("is_admin") != 0,
        })
    }

    fn is_rate_limited(&self, username: &str) -> bool {
        let mut attempts = self
            .login_attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match attempts.get(username) {
            Some((count, started)) if now_secs() - started < LOGIN_WINDOW_SECS => {
                *count >= LOGIN_MAX_ATTEMPTS
            }
            Some(_) => {
                attempts.remove(username);
                false
            }
            None => false,
        }
    }

    fn record_failure(&self, username: &str) {
        let now = now_secs();
        let mut attempts = self
            .login_attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = attempts.entry(username.to_string()).or_insert((0, now));
        if now - entry.1 >= LOGIN_WINDOW_SECS {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    fn clear_failures(&self, username: &str) {
        self.login_attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(username);
    }

    // ---- session cookies ----------------------------------------------------

    pub async fn make_session_cookie(&self, username: &str) -> anyhow::Result<String> {
        let epoch = self.session_epoch(username).await?.unwrap_or(1);
        let issued_at = now_secs();
        let signature = self.sign(username, issued_at, epoch);
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        Ok(format!(
            "{SESSION_COOKIE}={username}:{issued_at}:{epoch}:{signature}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
            self.session_max_age
        ))
    }

    pub fn clear_session_cookie(&self) -> String {
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}")
    }

    /// Validates a cookie header and returns the session it identifies.
    ///
    /// A cookie is rejected once the user is gone, their `session_epoch` has
    /// moved on (password change or an admin revoking access), or it is older
    /// than `session_max_age`.
    pub async fn session_from_cookie(&self, cookie_header: &str) -> Option<Session> {
        let value = cookie_header
            .split(';')
            .map(str::trim)
            .find_map(|cookie| cookie.strip_prefix(&format!("{SESSION_COOKIE}=")))?;

        let mut parts = value.splitn(4, ':');
        let username = parts.next()?;
        let issued_at: i64 = parts.next()?.parse().ok()?;
        let epoch: i64 = parts.next()?.parse().ok()?;
        let signature = hex::decode(parts.next()?).ok()?;

        let age = now_secs().saturating_sub(issued_at);
        if age < 0 || age > self.session_max_age {
            return None;
        }

        let mut mac = HmacSha256::new_from_slice(&self.session_secret).ok()?;
        mac.update(session_payload(username, issued_at, epoch).as_bytes());
        mac.verify_slice(&signature).ok()?;

        let row = sqlx::query("SELECT is_admin, session_epoch FROM users WHERE username = ?1")
            .bind(username)
            .fetch_optional(&self.users)
            .await
            .ok()??;
        if row.get::<i64, _>("session_epoch") != epoch {
            return None;
        }

        Some(Session {
            username: username.to_string(),
            is_admin: row.get::<i64, _>("is_admin") != 0,
        })
    }

    async fn session_epoch(&self, username: &str) -> anyhow::Result<Option<i64>> {
        let row = sqlx::query("SELECT session_epoch FROM users WHERE username = ?1")
            .bind(username)
            .fetch_optional(&self.users)
            .await?;
        Ok(row.map(|row| row.get::<i64, _>("session_epoch")))
    }

    fn sign(&self, username: &str, issued_at: i64, epoch: i64) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.session_secret)
            .expect("HMAC accepts a key of any length");
        mac.update(session_payload(username, issued_at, epoch).as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    // ---- bootstrap ----------------------------------------------------------

    /// Seeds accounts from the environment on first run.
    ///
    /// Existing users are never overwritten, so a password changed in the admin
    /// UI survives a restart even if the old value is still in the compose file.
    async fn seed_users_from_env(&self) -> anyhow::Result<()> {
        let admins: Vec<String> = env::var("EISENPOWER_ADMINS")
            .unwrap_or_default()
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();

        let raw_users = env::var("EISENPOWER_USERS")
            .or_else(|_| env::var("EISENHOWER_USERS"))
            .unwrap_or_default();

        for raw_pair in raw_users.split(',') {
            let pair = raw_pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((username, password)) = pair.split_once(':') else {
                anyhow::bail!(
                    "invalid EISENPOWER_USERS entry '{pair}', expected username:password"
                );
            };
            let username = username.trim();
            if !is_valid_username(username) {
                anyhow::bail!(
                    "invalid username '{username}', use letters, numbers, '.', '-', or '_'"
                );
            }
            if self.find_user(username).await?.is_some() {
                continue;
            }
            let is_admin = admins.iter().any(|name| name == username);
            self.create_user(username, password, is_admin)
                .await
                .with_context(|| format!("seeding user '{username}' from EISENPOWER_USERS"))?;
            tracing::info!(username, is_admin, "seeded user from EISENPOWER_USERS");
        }

        // Promote any named admin that already existed without the flag.
        for username in &admins {
            if let Some(user) = self.find_user(username).await? {
                if !user.is_admin {
                    self.set_admin(username, true).await?;
                }
            }
        }

        if self.admin_count().await? == 0 {
            let username =
                env::var("EISENPOWER_ADMIN_USERNAME").unwrap_or_else(|_| "admin".to_string());
            let password = env::var("EISENPOWER_ADMIN_PASSWORD")
                .unwrap_or_else(|_| "changeme-please".to_string());
            match self.find_user(&username).await? {
                Some(_) => self.set_admin(&username, true).await?,
                None => self.create_user(&username, &password, true).await?,
            }
            tracing::warn!(
                username,
                "no administrator existed; created one. Sign in and change the password."
            );
        }

        Ok(())
    }
}

fn session_payload(username: &str, issued_at: i64, epoch: i64) -> String {
    format!("{SESSION_VERSION}|{username}|{issued_at}|{epoch}")
}

#[derive(Debug)]
pub enum LoginError {
    InvalidCredentials,
    RateLimited,
    Internal(anyhow::Error),
}

async fn init_users_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            username TEXT PRIMARY KEY,
            password_hash TEXT NOT NULL,
            is_admin INTEGER NOT NULL DEFAULT 0,
            session_epoch INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Reads the session secret, generating and persisting a random one if needed.
///
/// A random persisted secret keeps cookies valid across restarts without the
/// operator having to invent one, and without the secret being derivable from
/// public information.
async fn load_or_create_session_secret(auth_dir: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    if let Ok(secret) =
        env::var("EISENPOWER_SESSION_SECRET").or_else(|_| env::var("EISENHOWER_SESSION_SECRET"))
    {
        let secret = secret.trim().to_string();
        if !secret.is_empty() {
            if secret.len() < 16 {
                tracing::warn!("EISENPOWER_SESSION_SECRET is short; use at least 32 characters");
            }
            return Ok(secret.into_bytes());
        }
    }

    let key_path = auth_dir.join("session.key");
    if let Ok(existing) = tokio::fs::read(&key_path).await {
        if existing.len() >= 32 {
            return Ok(existing);
        }
    }

    let mut secret = vec![0_u8; 32];
    OsRng.fill_bytes(&mut secret);
    tokio::fs::write(&key_path, &secret)
        .await
        .with_context(|| format!("writing {}", key_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .await
            .ok();
    }

    tracing::info!(path = %key_path.display(), "generated a new session secret");
    Ok(secret)
}
