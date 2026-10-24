//! Administrator screen for managing accounts while the server is running.

use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use crate::{auth::Session, html_escape, AppState};

/// Flash messages are carried in the query string so every action can simply
/// redirect back to the list.
#[derive(Deserialize, Default)]
pub struct Flash {
    ok: Option<String>,
    err: Option<String>,
}

#[derive(Deserialize)]
pub struct NewUserForm {
    username: String,
    password: String,
    is_admin: Option<String>,
}

#[derive(Deserialize)]
pub struct PasswordForm {
    password: String,
}

#[derive(Deserialize)]
pub struct AdminFlagForm {
    is_admin: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteUserForm {
    purge_data: Option<String>,
}

/// A browser omits an unchecked checkbox entirely, but the role form posts an
/// explicit empty value to revoke, so presence alone is not enough.
fn checked(value: &Option<String>) -> bool {
    matches!(value.as_deref(), Some("on" | "true" | "yes" | "1"))
}

/// Rejects anyone who is not a signed-in administrator.
async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<Session, Response> {
    let Some(session) = crate::current_session(state, headers).await else {
        return Err(Redirect::to("/login").into_response());
    };
    if !session.is_admin {
        return Err((StatusCode::FORBIDDEN, "Administrators only").into_response());
    }
    Ok(session)
}

fn back_to_admin(ok: Option<String>, err: Option<String>) -> Response {
    let query = match (ok, err) {
        (Some(message), _) => format!("?ok={}", urlencode(&message)),
        (None, Some(message)) => format!("?err={}", urlencode(&message)),
        (None, None) => String::new(),
    };
    Redirect::to(&format!("/admin{query}")).into_response()
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub async fn admin_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(flash): Query<Flash>,
) -> Response {
    let session = match require_admin(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };

    let users = match state.auth.list_users().await {
        Ok(users) => users,
        Err(err) => {
            tracing::error!(?err, "failed to list users");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to list users").into_response();
        }
    };
    let admin_count = users.iter().filter(|user| user.is_admin).count();

    let mut rows = String::new();
    for user in &users {
        let name = html_escape(&user.username);
        let is_self = user.username == session.username;
        let role = if user.is_admin {
            "Administrator"
        } else {
            "User"
        };
        let you = if is_self {
            " <span class='admin-badge'>you</span>"
        } else {
            ""
        };

        // Removing your own access, or the last administrator's, would lock
        // everyone out of this screen, so those controls are simply absent.
        let last_admin = user.is_admin && admin_count <= 1;
        let role_control = if is_self || last_admin {
            format!("<span class='muted small'>{role}</span>")
        } else {
            let target = if user.is_admin { "" } else { "on" };
            let label = if user.is_admin {
                "Revoke admin"
            } else {
                "Make admin"
            };
            format!(
                "<form method='post' action='/admin/users/{name}/admin' class='inline-form'>\
                 <input type='hidden' name='is_admin' value='{target}'>\
                 <button type='submit' class='link-btn'>{label}</button></form>"
            )
        };

        let delete_control = if is_self || last_admin {
            "<span class='muted small'>&mdash;</span>".to_string()
        } else {
            format!(
                "<form method='post' action='/admin/users/{name}/delete' class='inline-form' \
                 onsubmit=\"return confirm('Remove {name}? This signs them out immediately.')\">\
                 <label class='small purge-label'><input type='checkbox' name='purge_data'> also delete their tasks</label>\
                 <button type='submit' class='danger-btn'>Remove</button></form>"
            )
        };

        rows.push_str(&format!(
            r#"<tr>
    <td class="user-cell"><strong>{name}</strong>{you}<div class="muted small">created {created}</div></td>
    <td>{role}</td>
    <td>{role_control}</td>
    <td>
        <form method="post" action="/admin/users/{name}/password" class="inline-form">
            <input type="password" name="password" placeholder="New password" minlength="8" required>
            <button type="submit">Set</button>
        </form>
    </td>
    <td>{delete_control}</td>
</tr>"#,
            created = html_escape(&user.created_at),
        ));
    }

    let banner = if let Some(message) = flash.ok.as_deref() {
        format!("<p class='admin-flash ok'>{}</p>", html_escape(message))
    } else if let Some(message) = flash.err.as_deref() {
        format!("<p class='admin-flash err'>{}</p>", html_escape(message))
    } else {
        String::new()
    };

    Html(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
    <title>Eisenpower &middot; Users</title>
    <link rel="stylesheet" href="/static/style.css">
</head>
<body class="admin-page">
<header class="header">
    <div><strong>Eisenpower</strong> <span class="small muted">User administration</span></div>
    <div class="header-actions">
        <a class="button-link" href="/">Back to tasks</a>
        <form method="post" action="/logout"><button type="submit">Sign out</button></form>
    </div>
</header>
<main class="admin-main">
    {banner}
    <section class="admin-card">
        <h2>Accounts</h2>
        <table class="admin-table">
            <thead><tr><th>User</th><th>Role</th><th>Change role</th><th>Reset password</th><th>Remove</th></tr></thead>
            <tbody>{rows}</tbody>
        </table>
    </section>
    <section class="admin-card">
        <h2>Add a user</h2>
        <form method="post" action="/admin/users" class="admin-form">
            <label>Username
                <input type="text" name="username" pattern="[A-Za-z0-9._\-]+" required
                       title="Letters, numbers, '.', '-', or '_'">
            </label>
            <label>Password
                <input type="password" name="password" minlength="8" required>
            </label>
            <label class="checkbox-label">
                <input type="checkbox" name="is_admin"> Administrator
            </label>
            <button type="submit">Create user</button>
        </form>
        <p class="muted small">A new user starts with an empty task list. Their tasks are stored in
        their own database file, so removing them does not affect anyone else.</p>
    </section>
</main>
</body>
</html>"#
    ))
    .into_response()
}

pub async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewUserForm>,
) -> Response {
    if let Err(response) = require_admin(&state, &headers).await {
        return response;
    }
    let username = form.username.trim();
    match state
        .auth
        .create_user(username, &form.password, checked(&form.is_admin))
        .await
    {
        Ok(()) => back_to_admin(Some(format!("Created {username}.")), None),
        Err(err) => back_to_admin(None, Some(err.to_string())),
    }
}

pub async fn set_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Form(form): Form<PasswordForm>,
) -> Response {
    let session = match require_admin(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };

    if let Err(err) = state.auth.set_password(&username, &form.password).await {
        return back_to_admin(None, Some(err.to_string()));
    }

    // Changing a password bumps the session epoch, which invalidates every
    // cookie for that user -- including this one when an admin resets their own.
    if username == session.username {
        let mut response = Redirect::to("/login").into_response();
        if let Ok(value) = HeaderValue::from_str(&state.auth.clear_session_cookie()) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        return response;
    }

    back_to_admin(Some(format!("Password updated for {username}.")), None)
}

pub async fn set_admin_flag(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Form(form): Form<AdminFlagForm>,
) -> Response {
    let session = match require_admin(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if username == session.username {
        return back_to_admin(None, Some("You cannot change your own role.".to_string()));
    }

    let make_admin = checked(&form.is_admin);
    if !make_admin {
        match state.auth.admin_count().await {
            Ok(count) if count <= 1 => {
                return back_to_admin(
                    None,
                    Some("There must always be at least one administrator.".to_string()),
                )
            }
            Err(err) => {
                tracing::error!(?err, "failed to count administrators");
                return back_to_admin(None, Some("Unable to update the user.".to_string()));
            }
            _ => {}
        }
    }

    match state.auth.set_admin(&username, make_admin).await {
        Ok(()) => {
            let verb = if make_admin { "now" } else { "no longer" };
            back_to_admin(
                Some(format!("{username} is {verb} an administrator.")),
                None,
            )
        }
        Err(err) => back_to_admin(None, Some(err.to_string())),
    }
}

pub async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Form(form): Form<DeleteUserForm>,
) -> Response {
    let session = match require_admin(&state, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if username == session.username {
        return back_to_admin(
            None,
            Some("You cannot remove your own account.".to_string()),
        );
    }

    match state.auth.find_user(&username).await {
        Ok(Some(user)) if user.is_admin => match state.auth.admin_count().await {
            Ok(count) if count <= 1 => {
                return back_to_admin(
                    None,
                    Some("There must always be at least one administrator.".to_string()),
                )
            }
            Err(err) => {
                tracing::error!(?err, "failed to count administrators");
                return back_to_admin(None, Some("Unable to remove the user.".to_string()));
            }
            _ => {}
        },
        Ok(None) => return back_to_admin(None, Some(format!("No user named '{username}'."))),
        Err(err) => {
            tracing::error!(?err, "failed to look up user");
            return back_to_admin(None, Some("Unable to remove the user.".to_string()));
        }
        _ => {}
    }

    if let Err(err) = state.auth.delete_user(&username).await {
        return back_to_admin(None, Some(err.to_string()));
    }

    // Drop the cached pool so the database file is no longer held open.
    let pool = state
        .pools
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&username);
    if let Some(pool) = pool {
        pool.close().await;
    }

    let mut message = format!("Removed {username}.");
    if checked(&form.purge_data) {
        let path = state.auth.user_db_path(&username);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => message.push_str(" Their tasks were deleted."),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::error!(?err, path = %path.display(), "failed to delete user database");
                message.push_str(" Their task database could not be deleted.");
            }
        }
        // SQLite may leave a write-ahead log and shared-memory file behind.
        for suffix in ["-wal", "-shm"] {
            let sidecar = path.with_file_name(format!(
                "{}{suffix}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            tokio::fs::remove_file(sidecar).await.ok();
        }
    } else {
        message.push_str(" Their tasks were kept.");
    }

    back_to_admin(Some(message), None)
}
