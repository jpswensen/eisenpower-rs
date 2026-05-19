use axum::{
    extract::{Form, Json, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, patch, post},
    Router,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
    Row, SqlitePool,
};
use std::{
    collections::{BTreeMap, HashMap},
    env,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
};
use tower_http::services::ServeDir;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

const SESSION_COOKIE: &str = "eisenpower_session";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct AppState {
    auth: Arc<AuthConfig>,
    pools: Arc<Mutex<HashMap<String, SqlitePool>>>,
}

struct AuthConfig {
    users: HashMap<String, String>,
    data_dir: PathBuf,
    session_secret: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum TaskType {
    UrgentImportant,
    UrgentNotImportant,
    NotUrgentImportant,
    NotUrgentNotImportant,
}

impl TaskType {
    fn from_bucket(bucket: Bucket) -> TaskType {
        match bucket {
            Bucket::UrgentImportant => TaskType::UrgentImportant,
            Bucket::UrgentNotImportant => TaskType::UrgentNotImportant,
            Bucket::NotUrgentImportant => TaskType::NotUrgentImportant,
            Bucket::NotUrgentNotImportant => TaskType::NotUrgentNotImportant,
            Bucket::Today => TaskType::UrgentImportant,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            TaskType::UrgentImportant => "UrgentImportant",
            TaskType::UrgentNotImportant => "UrgentNotImportant",
            TaskType::NotUrgentImportant => "NotUrgentImportant",
            TaskType::NotUrgentNotImportant => "NotUrgentNotImportant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum Bucket {
    UrgentImportant,
    UrgentNotImportant,
    NotUrgentImportant,
    NotUrgentNotImportant,
    Today,
}

impl Bucket {
    fn as_str(&self) -> &'static str {
        match self {
            Bucket::UrgentImportant => "UrgentImportant",
            Bucket::UrgentNotImportant => "UrgentNotImportant",
            Bucket::NotUrgentImportant => "NotUrgentImportant",
            Bucket::NotUrgentNotImportant => "NotUrgentNotImportant",
            Bucket::Today => "Today",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Task {
    id: i64,
    title: String,
    task_type: TaskType,
    bucket: Bucket,
    completed: bool,
    position: i64,
    parent_id: Option<i64>,
    collapsed: bool,
    created_at: String,
    updated_at: String,
    children: Vec<Task>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();

    let state = AppState {
        auth: Arc::new(AuthConfig::from_env()?),
        pools: Arc::new(Mutex::new(HashMap::new())),
    };

    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    let app = Router::new()
        .route("/", get(index))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/tasks", post(add_task))
        .route("/tasks/{id}", patch(update_task))
        .route("/tasks/{id}/collapse", post(toggle_collapse))
        .route("/tasks/{id}/delete", post(delete_task))
        .route("/tasks/{id}/subtasks", post(add_subtask))
        .route("/tasks/{id}/toggle", post(toggle_task))
        .route("/reorder", post(reorder_tasks))
        .route("/move", post(move_task))
        .route("/completed", get(completed_tasks))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(?addr, "listening");
    axum::serve(listener, app).await?;

    Ok(())
}

impl AuthConfig {
    fn from_env() -> anyhow::Result<Self> {
        let users = load_users_from_env()?;
        let data_dir = env::var("EISENPOWER_DATA_DIR")
            .or_else(|_| env::var("EISENHOWER_DATA_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));

        let session_secret = env::var("EISENPOWER_SESSION_SECRET")
            .or_else(|_| env::var("EISENHOWER_SESSION_SECRET"))
            .unwrap_or_else(|_| derived_dev_secret(&users, &data_dir))
            .into_bytes();

        Ok(Self {
            users,
            data_dir,
            session_secret,
        })
    }

    fn user_db_path(&self, username: &str) -> PathBuf {
        self.data_dir.join(format!("{username}.db"))
    }
}

fn load_users_from_env() -> anyhow::Result<HashMap<String, String>> {
    if let Ok(raw_users) = env::var("EISENPOWER_USERS").or_else(|_| env::var("EISENHOWER_USERS")) {
        let mut users = HashMap::new();
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
            users.insert(username.to_string(), password.to_string());
        }
        if users.is_empty() {
            anyhow::bail!("EISENPOWER_USERS did not contain any users");
        }
        return Ok(users);
    }

    let username = env::var("EISENPOWER_USERNAME")
        .or_else(|_| env::var("EISENHOWER_USERNAME"))
        .unwrap_or_else(|_| "admin".to_string());
    let password = env::var("EISENPOWER_PASSWORD")
        .or_else(|_| env::var("EISENHOWER_PASSWORD"))
        .unwrap_or_else(|_| "password".to_string());

    if !is_valid_username(&username) {
        anyhow::bail!("invalid username '{username}', use letters, numbers, '.', '-', or '_'");
    }

    Ok(HashMap::from([(username, password)]))
}

fn derived_dev_secret(users: &HashMap<String, String>, data_dir: &FsPath) -> String {
    let mut pairs = users
        .iter()
        .map(|(username, password)| format!("{username}:{password}"))
        .collect::<Vec<_>>();
    pairs.sort();
    format!(
        "eisenpower-dev-secret:{}:{}",
        data_dir.display(),
        pairs.join("|")
    )
}

fn is_valid_username(username: &str) -> bool {
    !username.is_empty()
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

async fn pool_for_user(state: &AppState, username: &str) -> anyhow::Result<SqlitePool> {
    if let Some(pool) = state
        .pools
        .lock()
        .expect("pool registry mutex poisoned")
        .get(username)
        .cloned()
    {
        return Ok(pool);
    }

    tokio::fs::create_dir_all(&state.auth.data_dir).await?;
    let db_path = state.auth.user_db_path(username);
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    init_schema(&pool).await?;

    state
        .pools
        .lock()
        .expect("pool registry mutex poisoned")
        .insert(username.to_string(), pool.clone());

    Ok(pool)
}

async fn init_schema(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            task_type TEXT NOT NULL CHECK(task_type IN ('UrgentImportant','UrgentNotImportant','NotUrgentImportant','NotUrgentNotImportant')),
            bucket TEXT NOT NULL CHECK(bucket IN ('UrgentImportant','UrgentNotImportant','NotUrgentImportant','NotUrgentNotImportant','Today')),
            completed INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL DEFAULT 0,
            parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
            collapsed INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await?;

    add_column_if_missing(
        pool,
        "parent_id",
        "ALTER TABLE tasks ADD COLUMN parent_id INTEGER",
    )
    .await?;
    add_column_if_missing(
        pool,
        "collapsed",
        "ALTER TABLE tasks ADD COLUMN collapsed INTEGER NOT NULL DEFAULT 1",
    )
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_tasks_bucket_position ON tasks(bucket, position)")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_tasks_parent_position ON tasks(parent_id, position)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn add_column_if_missing(pool: &SqlitePool, column: &str, ddl: &str) -> anyhow::Result<()> {
    let columns = sqlx::query("PRAGMA table_info(tasks)")
        .fetch_all(pool)
        .await?;
    let exists = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == column);
    if !exists {
        sqlx::query(ddl).execute(pool).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn login_page() -> Html<String> {
    Html(render_login(None))
}

async fn login(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    let username = form.username.trim();
    let valid = state
        .auth
        .users
        .get(username)
        .is_some_and(|password| password == &form.password);

    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            Html(render_login(Some("Invalid username or password"))),
        )
            .into_response();
    }

    if let Err(err) = pool_for_user(&state, username).await {
        tracing::error!(?err, username, "failed to initialize user database");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to initialize the user database",
        )
            .into_response();
    }

    let mut response = Redirect::to("/").into_response();
    if let Ok(value) = HeaderValue::from_str(&make_session_cookie(username, &state.auth)) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

async fn logout() -> Response {
    let mut response = Redirect::to("/login").into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("eisenpower_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    response
}

fn render_login(error: Option<&str>) -> String {
    let error_html = error
        .map(|message| format!("<p class='login-error'>{}</p>", html_escape(message)))
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
    <title>Eisenpower Login</title>
    <link rel="stylesheet" href="/static/style.css">
</head>
<body class="login-page">
    <main class="login-card">
        <h1>Eisenpower</h1>
        <p class="muted">Sign in to open your task database.</p>
        {error_html}
        <form method="post" action="/login" class="login-form">
            <label>Username
                <input type="text" name="username" autocomplete="username" autofocus required>
            </label>
            <label>Password
                <input type="password" name="password" autocomplete="current-password" required>
            </label>
            <button type="submit">Sign in</button>
        </form>
    </main>
</body>
</html>"#
    )
}

fn make_session_cookie(username: &str, auth: &AuthConfig) -> String {
    let signature = sign_username(username, auth);
    format!("{SESSION_COOKIE}={username}.{signature}; Path=/; HttpOnly; SameSite=Lax")
}

fn sign_username(username: &str, auth: &AuthConfig) -> String {
    let mut mac =
        HmacSha256::new_from_slice(&auth.session_secret).expect("HMAC accepts any key length");
    mac.update(username.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn authenticated_user(headers: &HeaderMap, auth: &AuthConfig) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    let value = cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix(&format!("{SESSION_COOKIE}=")))?;
    let (username, signature) = value.rsplit_once('.')?;
    if !auth.users.contains_key(username) {
        return None;
    }

    let signature = hex::decode(signature).ok()?;
    let mut mac = HmacSha256::new_from_slice(&auth.session_secret).ok()?;
    mac.update(username.as_bytes());
    mac.verify_slice(&signature).ok()?;
    Some(username.to_string())
}

async fn require_pool(state: &AppState, headers: &HeaderMap) -> Result<SqlitePool, Response> {
    let Some(username) = authenticated_user(headers, &state.auth) else {
        return Err(StatusCode::UNAUTHORIZED.into_response());
    };

    pool_for_user(state, &username).await.map_err(|err| {
        tracing::error!(?err, username, "failed to open user database");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to open the user database",
        )
            .into_response()
    })
}

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(username) = authenticated_user(&headers, &state.auth) else {
        return Redirect::to("/login").into_response();
    };

    let pool = match pool_for_user(&state, &username).await {
        Ok(pool) => pool,
        Err(err) => {
            tracing::error!(?err, username, "failed to open user database");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to open the user database",
            )
                .into_response();
        }
    };

    match fetch_all_grouped(&pool).await {
        Ok(groups) => Html(render_index(&username, groups)).into_response(),
        Err(err) => {
            tracing::error!(?err, username, "failed to load tasks");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to load tasks").into_response()
        }
    }
}

fn render_index(username: &str, groups: BTreeMap<&'static str, Vec<Task>>) -> String {
    let ui = groups.get("UrgentImportant").cloned().unwrap_or_default();
    let uni = groups
        .get("UrgentNotImportant")
        .cloned()
        .unwrap_or_default();
    let nui = groups
        .get("NotUrgentImportant")
        .cloned()
        .unwrap_or_default();
    let nun = groups
        .get("NotUrgentNotImportant")
        .cloned()
        .unwrap_or_default();
    let today = groups.get("Today").cloned().unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
    <title>Eisenpower</title>
    <link rel="stylesheet" href="/static/style.css">
    <script src="https://unpkg.com/htmx.org@1.9.10"></script>
    <script src="https://cdn.jsdelivr.net/npm/sortablejs@1.15.2/Sortable.min.js"></script>
</head>
<body>
<div class="matrix-scroll">
    <header class="header">
        <div>
            <strong>Eisenpower</strong>
            <span class="small muted">Signed in as {}</span>
        </div>
        <div class="header-actions">
            <button id="refresh-btn" type="button" tabindex="-1">Refresh</button>
            <button id="show-completed-btn" type="button" tabindex="-1">Completed Tasks</button>
            <form method="post" action="/logout">
                <button type="submit" tabindex="-1">Sign out</button>
            </form>
        </div>
    </header>
    <main class="grid">
        <section class="column ui">
            <div class="column-title">Urgent & Important</div>
            {}
        </section>
        <section class="column uni">
            <div class="column-title">Urgent & Not Important</div>
            {}
        </section>
        <section class="column today">
            <div class="column-title">Today's Tasks</div>
            {}
        </section>
        <section class="column nui">
            <div class="column-title">Not Urgent & Important</div>
            {}
        </section>
        <section class="column nun">
            <div class="column-title">Not Urgent & Not Important</div>
            {}
        </section>
    </main>
    <aside id="completed-panel" class="completed-panel" hidden>
        <div class="completed-panel-content">
            <button id="close-completed-btn" type="button" class="panel-close" tabindex="-1">Close</button>
            <h2>Completed Tasks</h2>
            <div id="completed-tasks-list" hx-get="/completed" hx-trigger="revealed" hx-swap="innerHTML"></div>
        </div>
    </aside>
</div>
<script>
function directIds(el, selector) {{
    return Array.from(el.querySelectorAll(':scope > ' + selector)).map(item => Number(item.dataset.id));
}}

async function postJson(url, body) {{
    const response = await fetch(url, {{
        method: 'POST',
        headers: {{'Content-Type': 'application/json'}},
        body: JSON.stringify(body)
    }});
    if (!response.ok) {{
        window.location.reload();
        return null;
    }}
    return response;
}}

function replaceDragged(item, html) {{
    const temp = document.createElement('template');
    temp.innerHTML = html.trim();
    const replacements = Array.from(temp.content.children);
    if (replacements.length === 0) {{
        item.remove();
        return;
    }}
    item.replaceWith(...replacements);
    if (window.htmx) {{
        replacements.forEach((replacement) => window.htmx.process(replacement));
    }}
    initSortables();
}}

let activeDragItem = null;
let nestingCandidate = null;
const NESTING_EDGE_RATIO = 0.10;

function setNestingCandidate(task) {{
    if (nestingCandidate === task) return;
    clearNestingCandidate();
    nestingCandidate = task;
    if (nestingCandidate) nestingCandidate.classList.add('nesting-candidate');
}}

function clearNestingCandidate() {{
    if (nestingCandidate) nestingCandidate.classList.remove('nesting-candidate');
    nestingCandidate = null;
}}

function dragPoint(event) {{
    if (!event) return null;
    if (event.touches && event.touches.length > 0) return event.touches[0];
    if (event.changedTouches && event.changedTouches.length > 0) return event.changedTouches[0];
    if (Number.isFinite(event.clientX) && Number.isFinite(event.clientY)) return event;
    return null;
}}

function updateNestingCandidate(event) {{
    if (!activeDragItem) return;
    const point = dragPoint(event);
    if (!point) return;
    let candidate = null;
    document.querySelectorAll('.top-task').forEach((task) => {{
        if (candidate || task === activeDragItem || activeDragItem.contains(task)) return;
        const row = task.querySelector(':scope > .task-row');
        if (!row) return;
        const rect = row.getBoundingClientRect();
        if (point.clientX < rect.left || point.clientX > rect.right || point.clientY < rect.top || point.clientY > rect.bottom) return;
        const yRatio = (point.clientY - rect.top) / rect.height;
        if (yRatio > NESTING_EDGE_RATIO && yRatio < 1 - NESTING_EDGE_RATIO) {{
            candidate = task;
        }}
    }});
    setNestingCandidate(candidate);
    return candidate;
}}

function appendDraggedToParent(item, parentTask, html) {{
    const targetList = parentTask.querySelector(':scope > .subtask-region > .subtask-list');
    if (!targetList) {{
        replaceDragged(item, html);
        return null;
    }}
    const temp = document.createElement('template');
    temp.innerHTML = html.trim();
    const replacements = Array.from(temp.content.children);
    item.remove();
    targetList.append(...replacements);
    if (window.htmx) {{
        replacements.forEach((replacement) => window.htmx.process(replacement));
    }}
    expandDropParent(targetList);
    initSortables();
    return targetList;
}}

function expandDropParent(list) {{
    const parentTask = list.closest('.top-task');
    if (!parentTask) return;
    parentTask.classList.remove('is-collapsed');
    const collapseButton = parentTask.querySelector(':scope > .task-row .collapse-btn');
    if (collapseButton) collapseButton.innerHTML = '&#9662;';
}}

async function reorderList(list) {{
    if (list.classList.contains('tasklist')) {{
        await postJson('/reorder', {{
            bucket: list.dataset.bucket,
            orderedIds: directIds(list, '.top-task')
        }});
        return;
    }}
    if (list.classList.contains('subtask-list')) {{
        await postJson('/reorder', {{
            parentId: Number(list.dataset.parentId),
            orderedIds: directIds(list, '.subtask')
        }});
    }}
}}

function initSortables() {{
    document.querySelectorAll('.tasklist').forEach((el) => {{
        if (el.dataset.sortableReady) return;
        el.dataset.sortableReady = '1';
        new Sortable(el, {{
            animation: 150,
            group: {{ name: 'matrix', pull: true, put: true }},
            draggable: '.top-task, .subtask',
            delay: 250,
            delayOnTouchOnly: true,
            emptyInsertThreshold: 48,
            swapThreshold: 0.35,
            onStart: function(evt) {{
                activeDragItem = evt.item;
                document.body.classList.add('dragging-task');
            }},
            onMove: function(evt) {{
                return updateNestingCandidate(evt.originalEvent) ? false : true;
            }},
            onEnd: async function(evt) {{
                updateNestingCandidate(evt.originalEvent);
                const candidate = nestingCandidate;
                clearNestingCandidate();
                document.body.classList.remove('dragging-task');
                activeDragItem = null;
                const id = Number(evt.item.dataset.id);
                if (candidate) {{
                    const response = await postJson('/move', {{
                        id: id,
                        parentId: Number(candidate.dataset.id),
                        index: candidate.querySelectorAll(':scope > .subtask-region > .subtask-list > .subtask').length
                    }});
                    if (!response) return;
                    const targetList = appendDraggedToParent(evt.item, candidate, await response.text());
                    if (targetList) await reorderList(targetList);
                    if (evt.from !== targetList) await reorderList(evt.from);
                    return;
                }}
                if (evt.to.classList.contains('subtask-list')) {{
                    const response = await postJson('/move', {{
                        id: id,
                        parentId: Number(evt.to.dataset.parentId),
                        index: evt.newDraggableIndex ?? evt.newIndex
                    }});
                    if (!response) return;
                    expandDropParent(evt.to);
                    replaceDragged(evt.item, await response.text());
                    await reorderList(evt.to);
                    if (evt.from !== evt.to) await reorderList(evt.from);
                    return;
                }}

                const response = await postJson('/move', {{
                    id: id,
                    bucket: evt.to.dataset.bucket,
                    parentId: null,
                    index: evt.newDraggableIndex ?? evt.newIndex
                }});
                if (!response) return;
                replaceDragged(evt.item, await response.text());
                await reorderList(evt.to);
                if (evt.from !== evt.to) await reorderList(evt.from);
            }}
        }});
    }});

    document.querySelectorAll('.subtask-list').forEach((el) => {{
        if (el.dataset.sortableReady) return;
        el.dataset.sortableReady = '1';
        new Sortable(el, {{
            animation: 150,
            group: {{ name: 'matrix', pull: true, put: true }},
            draggable: '.subtask',
            delay: 250,
            delayOnTouchOnly: true,
            emptyInsertThreshold: 48,
            swapThreshold: 0.35,
            onStart: function(evt) {{
                activeDragItem = evt.item;
                document.body.classList.add('dragging-task');
            }},
            onMove: function(evt) {{
                return updateNestingCandidate(evt.originalEvent) ? false : true;
            }},
            onEnd: async function(evt) {{
                updateNestingCandidate(evt.originalEvent);
                const candidate = nestingCandidate;
                clearNestingCandidate();
                document.body.classList.remove('dragging-task');
                activeDragItem = null;
                if (candidate) {{
                    const response = await postJson('/move', {{
                        id: Number(evt.item.dataset.id),
                        parentId: Number(candidate.dataset.id),
                        index: candidate.querySelectorAll(':scope > .subtask-region > .subtask-list > .subtask').length
                    }});
                    if (!response) return;
                    const targetList = appendDraggedToParent(evt.item, candidate, await response.text());
                    if (targetList) await reorderList(targetList);
                    if (evt.from !== targetList) await reorderList(evt.from);
                    return;
                }}
                if (evt.to.classList.contains('tasklist')) {{
                    const response = await postJson('/move', {{
                        id: Number(evt.item.dataset.id),
                        bucket: evt.to.dataset.bucket,
                        parentId: null,
                        index: evt.newDraggableIndex ?? evt.newIndex
                    }});
                    if (!response) return;
                    replaceDragged(evt.item, await response.text());
                    await reorderList(evt.to);
                    if (evt.from !== evt.to) await reorderList(evt.from);
                    return;
                }}

                const response = await postJson('/move', {{
                    id: Number(evt.item.dataset.id),
                    parentId: Number(evt.to.dataset.parentId),
                    index: evt.newDraggableIndex ?? evt.newIndex
                }});
                if (!response) return;
                expandDropParent(evt.to);
                replaceDragged(evt.item, await response.text());
                await reorderList(evt.to);
            }}
        }});
    }});
}}

document.addEventListener('DOMContentLoaded', initSortables);
document.body.addEventListener('htmx:afterSwap', initSortables);
document.addEventListener('pointermove', updateNestingCandidate);
document.addEventListener('mousemove', updateNestingCandidate);
document.addEventListener('touchmove', updateNestingCandidate, {{ passive: true }});

let lastScrollX = 0;
document.body.addEventListener('htmx:beforeSwap', function() {{
    const ms = document.querySelector('.matrix-scroll');
    if (ms) lastScrollX = ms.scrollLeft;
}});
document.body.addEventListener('htmx:afterSwap', function() {{
    const ms = document.querySelector('.matrix-scroll');
    if (ms) ms.scrollLeft = lastScrollX;
}});

document.getElementById('show-completed-btn').onclick = function() {{
    const panel = document.getElementById('completed-panel');
    panel.hidden = false;
    document.getElementById('completed-tasks-list').dispatchEvent(new Event('revealed'));
}};
document.getElementById('close-completed-btn').onclick = function() {{
    document.getElementById('completed-panel').hidden = true;
    window.location.reload();
}};
document.getElementById('refresh-btn').onclick = function() {{
    const ms = document.querySelector('.matrix-scroll');
    if (ms) sessionStorage.setItem('matrixScrollX', ms.scrollLeft);
    window.location.reload();
}};
document.addEventListener('DOMContentLoaded', function() {{
    const ms = document.querySelector('.matrix-scroll');
    const scrollX = sessionStorage.getItem('matrixScrollX');
    if (ms && scrollX) {{
        ms.scrollLeft = parseInt(scrollX, 10);
        sessionStorage.removeItem('matrixScrollX');
    }}
}});
</script>
</body>
</html>"#,
        html_escape(username),
        render_column("UrgentImportant", "list-UI", &ui, 3, 4),
        render_column("UrgentNotImportant", "list-UNI", &uni, 5, 6),
        render_column("Today", "list-TODAY", &today, 1, 2),
        render_column("NotUrgentImportant", "list-NUI", &nui, 7, 8),
        render_column("NotUrgentNotImportant", "list-NUN", &nun, 9, 10),
    )
}

fn render_column(
    bucket: &str,
    list_id: &str,
    tasks: &[Task],
    input_tabindex: i32,
    button_tabindex: i32,
) -> String {
    let mut html = format!(
        r##"
<form class="add-form" hx-post="/tasks" hx-target="#{list_id}" hx-swap="beforeend" hx-on::after-request="this.reset()">
    <input type="hidden" name="bucket" value="{bucket}">
    <input type="text" name="title" placeholder="New task..." autocomplete="off" tabindex="{input_tabindex}" required>
    <button type="submit" tabindex="{button_tabindex}">Add</button>
</form>"##
    );
    html.push_str(&format!(
        r#"<ul class="tasklist" id="{list_id}" data-bucket="{bucket}">"#
    ));
    for task in tasks {
        html.push_str(&render_task(task));
    }
    html.push_str("</ul>");
    html
}

fn render_task(task: &Task) -> String {
    let title = html_escape(&task.title);
    let chip = color_chip(task);
    let collapsed_class = if task.collapsed { " is-collapsed" } else { "" };
    let collapse_symbol = if task.collapsed { "&#9656;" } else { "&#9662;" };
    let completion_button = completion_button(task, "closest li.task", true);
    let delete_button = delete_button(task.id, "closest li.task");
    let mut children = String::new();
    for child in &task.children {
        children.push_str(&render_subtask(child));
    }

    format!(
        r##"<li class="task top-task{collapsed_class}" data-id="{id}">
    <div class="task-row">
        <div class="color-chip {chip}"></div>
        <button class="collapse-btn" type="button" tabindex="-1" hx-post="/tasks/{id}/collapse" hx-target="closest li.task" hx-swap="outerHTML" title="Expand or collapse subtasks">{collapse_symbol}</button>
        <div class="text" contenteditable="true" tabindex="-1" onblur="fetch('/tasks/{id}', {{method:'PATCH', headers:{{'Content-Type':'application/json'}}, body: JSON.stringify({{title:this.innerText}})}})">{title}</div>
        <div class="controls">{completion_button}{delete_button}</div>
    </div>
    <div class="subtask-region">
        <ul class="subtask-list" id="subtasks-{id}" data-parent-id="{id}">{children}</ul>
        <form class="add-subtask-form" hx-post="/tasks/{id}/subtasks" hx-target="#subtasks-{id}" hx-swap="beforeend" hx-on::after-request="this.reset()">
            <input type="text" name="title" placeholder="Add a subtask..." autocomplete="off" tabindex="-1" required>
            <button type="submit" tabindex="-1">Add subtask</button>
        </form>
    </div>
</li>"##,
        id = task.id,
    )
}

fn render_subtask(task: &Task) -> String {
    let title = html_escape(&task.title);
    let completed_class = if task.completed {
        " subtask-completed"
    } else {
        ""
    };
    let completion_button = completion_button(task, "closest li.subtask", false);
    let delete_button = delete_button(task.id, "closest li.subtask");

    format!(
        r#"<li class="subtask{completed_class}" data-id="{id}">
    <span class="drag-handle" title="Drag to reorder or promote" aria-hidden="true"></span>
    <div class="subtask-text" contenteditable="true" tabindex="-1" onblur="fetch('/tasks/{id}', {{method:'PATCH', headers:{{'Content-Type':'application/json'}}, body: JSON.stringify({{title:this.innerText}})}})">{title}</div>
    <div class="controls">{completion_button}{delete_button}</div>
</li>"#,
        id = task.id,
    )
}

fn completion_button(task: &Task, target: &str, reload_after_swap: bool) -> String {
    let (class, title, icon) = if task.completed {
        ("undo-btn", "Undo", "&#8634;")
    } else {
        ("done-btn", "Done", "&#10003;")
    };
    let after_swap = if reload_after_swap {
        " hx-on::afterSwap='window.location.reload()'"
    } else {
        ""
    };
    format!(
        "<button class='{class}' type='button' tabindex='-1' hx-post='/tasks/{}/toggle' hx-target='{target}' hx-swap='outerHTML'{after_swap} title='{title}'>{icon}</button>",
        task.id
    )
}

fn delete_button(id: i64, target: &str) -> String {
    format!(
        "<button class='delete-btn' type='button' tabindex='-1' hx-post='/tasks/{id}/delete' hx-target='{target}' hx-swap='outerHTML' title='Delete'>&times;</button>"
    )
}

fn color_chip(task: &Task) -> &'static str {
    match task.bucket {
        Bucket::Today => match task.task_type {
            TaskType::UrgentImportant => "color-UI",
            TaskType::UrgentNotImportant => "color-UNI",
            TaskType::NotUrgentImportant => "color-NUI",
            TaskType::NotUrgentNotImportant => "color-NUN",
        },
        Bucket::UrgentImportant => "color-UI",
        Bucket::UrgentNotImportant => "color-UNI",
        Bucket::NotUrgentImportant => "color-NUI",
        Bucket::NotUrgentNotImportant => "color-NUN",
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

async fn fetch_all_grouped(pool: &SqlitePool) -> anyhow::Result<BTreeMap<&'static str, Vec<Task>>> {
    let rows = sqlx::query(
        r#"
        SELECT id, title, task_type, bucket, completed, position, parent_id, collapsed, created_at, updated_at
        FROM tasks
        ORDER BY bucket, parent_id IS NOT NULL, parent_id, position ASC, id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut top_level = Vec::new();
    let mut children: HashMap<i64, Vec<Task>> = HashMap::new();
    for row in rows {
        let task = task_from_row(&row);
        if let Some(parent_id) = task.parent_id {
            children.entry(parent_id).or_default().push(task);
        } else {
            top_level.push(task);
        }
    }

    let mut groups: BTreeMap<&'static str, Vec<Task>> = BTreeMap::new();
    for mut task in top_level {
        task.children = children.remove(&task.id).unwrap_or_default();
        if !task.completed {
            groups.entry(task.bucket.as_str()).or_default().push(task);
        }
    }
    Ok(groups)
}

async fn fetch_task_with_children(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Task>> {
    let Some(row) = sqlx::query(
        r#"
        SELECT id, title, task_type, bucket, completed, position, parent_id, collapsed, created_at, updated_at
        FROM tasks
        WHERE id = ?1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let mut task = task_from_row(&row);
    let rows = sqlx::query(
        r#"
        SELECT id, title, task_type, bucket, completed, position, parent_id, collapsed, created_at, updated_at
        FROM tasks
        WHERE parent_id = ?1
        ORDER BY position ASC, id ASC
        "#,
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    task.children = rows.iter().map(task_from_row).collect();

    Ok(Some(task))
}

async fn fetch_task(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Task>> {
    let row = sqlx::query(
        r#"
        SELECT id, title, task_type, bucket, completed, position, parent_id, collapsed, created_at, updated_at
        FROM tasks
        WHERE id = ?1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.as_ref().map(task_from_row))
}

async fn require_task(pool: &SqlitePool, id: i64) -> Result<Task, Response> {
    match fetch_task(pool, id).await {
        Ok(Some(task)) => Ok(task),
        Ok(None) => Err((StatusCode::NOT_FOUND, "Task not found").into_response()),
        Err(err) => {
            tracing::error!(?err, id, "failed to load task");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "Unable to load task").into_response())
        }
    }
}

fn task_from_row(row: &SqliteRow) -> Task {
    Task {
        id: row.get("id"),
        title: row.get("title"),
        task_type: parse_task_type(&row.get::<String, _>("task_type"))
            .unwrap_or(TaskType::UrgentImportant),
        bucket: parse_bucket(&row.get::<String, _>("bucket")).unwrap_or(Bucket::UrgentImportant),
        completed: row.get::<i64, _>("completed") != 0,
        position: row.get("position"),
        parent_id: row.get("parent_id"),
        collapsed: row.get::<i64, _>("collapsed") != 0,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        children: Vec::new(),
    }
}

#[derive(Deserialize)]
struct NewTask {
    title: String,
    bucket: String,
}

#[derive(Deserialize)]
struct NewSubtask {
    title: String,
}

async fn add_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<NewTask>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };
    let title = form.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "Title required").into_response();
    }
    let Some(bucket) = parse_bucket(&form.bucket) else {
        return (StatusCode::BAD_REQUEST, "Invalid bucket").into_response();
    };
    let task_type = if matches!(bucket, Bucket::Today) {
        TaskType::UrgentImportant
    } else {
        TaskType::from_bucket(bucket)
    };
    let position = match next_position(&pool, bucket, None).await {
        Ok(position) => position,
        Err(err) => {
            tracing::error!(?err, "failed to choose task position");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to add task").into_response();
        }
    };

    let id = match sqlx::query(
        r#"
        INSERT INTO tasks(title, task_type, bucket, position, collapsed)
        VALUES (?1, ?2, ?3, ?4, 1)
        "#,
    )
    .bind(title)
    .bind(task_type.as_str())
    .bind(bucket.as_str())
    .bind(position)
    .execute(&pool)
    .await
    {
        Ok(result) => result.last_insert_rowid(),
        Err(err) => {
            tracing::error!(?err, "failed to add task");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to add task").into_response();
        }
    };

    match fetch_task_with_children(&pool, id).await {
        Ok(Some(task)) => Html(render_task(&task)).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to fetch new task");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to load task").into_response()
        }
    }
}

async fn add_subtask(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(parent_id): Path<i64>,
    Form(form): Form<NewSubtask>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };
    let title = form.title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "Title required").into_response();
    }

    let parent = match require_task(&pool, parent_id).await {
        Ok(task) => task,
        Err(response) => return response,
    };
    if parent.parent_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "Subtasks can only be added to top-level tasks",
        )
            .into_response();
    }

    let position = match next_position(&pool, parent.bucket, Some(parent.id)).await {
        Ok(position) => position,
        Err(err) => {
            tracing::error!(?err, parent_id, "failed to choose subtask position");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to add subtask").into_response();
        }
    };
    let id = match sqlx::query(
        r#"
        INSERT INTO tasks(title, task_type, bucket, position, parent_id)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
    )
    .bind(title)
    .bind(parent.task_type.as_str())
    .bind(parent.bucket.as_str())
    .bind(position)
    .bind(parent.id)
    .execute(&pool)
    .await
    {
        Ok(result) => result.last_insert_rowid(),
        Err(err) => {
            tracing::error!(?err, parent_id, "failed to add subtask");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to add subtask").into_response();
        }
    };

    if let Err(err) =
        sqlx::query("UPDATE tasks SET collapsed = 0, updated_at = datetime('now') WHERE id = ?1")
            .bind(parent.id)
            .execute(&pool)
            .await
    {
        tracing::error!(?err, parent_id, "failed to expand parent task");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to expand parent task",
        )
            .into_response();
    }

    match fetch_task(&pool, id).await {
        Ok(Some(task)) => Html(render_subtask(&task)).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to fetch new subtask");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to load subtask").into_response()
        }
    }
}

async fn next_position(
    pool: &SqlitePool,
    bucket: Bucket,
    parent_id: Option<i64>,
) -> anyhow::Result<i64> {
    let position = if let Some(parent_id) = parent_id {
        sqlx::query_as::<_, (i64,)>(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM tasks WHERE parent_id = ?1",
        )
        .bind(parent_id)
        .fetch_one(pool)
        .await?
        .0
    } else {
        sqlx::query_as::<_, (i64,)>(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM tasks WHERE bucket = ?1 AND parent_id IS NULL",
        )
        .bind(bucket.as_str())
        .fetch_one(pool)
        .await?
        .0
    };
    Ok(position)
}

fn parse_bucket(s: &str) -> Option<Bucket> {
    Some(match s {
        "UrgentImportant" => Bucket::UrgentImportant,
        "UrgentNotImportant" => Bucket::UrgentNotImportant,
        "NotUrgentImportant" => Bucket::NotUrgentImportant,
        "NotUrgentNotImportant" => Bucket::NotUrgentNotImportant,
        "Today" => Bucket::Today,
        _ => return None,
    })
}

fn parse_task_type(s: &str) -> Option<TaskType> {
    Some(match s {
        "UrgentImportant" => TaskType::UrgentImportant,
        "UrgentNotImportant" => TaskType::UrgentNotImportant,
        "NotUrgentImportant" => TaskType::NotUrgentImportant,
        "NotUrgentNotImportant" => TaskType::NotUrgentNotImportant,
        _ => return None,
    })
}

async fn delete_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    if let Err(err) = sqlx::query("DELETE FROM tasks WHERE parent_id = ?1")
        .bind(id)
        .execute(&pool)
        .await
    {
        tracing::error!(?err, id, "failed to delete child tasks");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to delete task").into_response();
    }
    if let Err(err) = sqlx::query("DELETE FROM tasks WHERE id = ?1")
        .bind(id)
        .execute(&pool)
        .await
    {
        tracing::error!(?err, id, "failed to delete task");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to delete task").into_response();
    }

    Html(String::new()).into_response()
}

async fn toggle_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    let task = match require_task(&pool, id).await {
        Ok(task) => task,
        Err(response) => return response,
    };
    let completed = !task.completed;
    if let Err(err) =
        sqlx::query("UPDATE tasks SET completed = ?1, updated_at = datetime('now') WHERE id = ?2")
            .bind(if completed { 1_i64 } else { 0_i64 })
            .bind(id)
            .execute(&pool)
            .await
    {
        tracing::error!(?err, id, "failed to toggle task");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to update task").into_response();
    }

    if task.parent_id.is_none() {
        if completed {
            if let Err(err) = sqlx::query(
                "UPDATE tasks SET completed = 1, updated_at = datetime('now') WHERE parent_id = ?1",
            )
            .bind(id)
            .execute(&pool)
            .await
            {
                tracing::error!(?err, id, "failed to complete subtasks");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to complete subtasks",
                )
                    .into_response();
            }
        }
        return Html(String::new()).into_response();
    }

    match fetch_task(&pool, id).await {
        Ok(Some(task)) => Html(render_subtask(&task)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Task not found").into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to reload task");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reload task").into_response()
        }
    }
}

#[derive(Deserialize)]
struct UpdateBody {
    title: Option<String>,
}

async fn update_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    let Some(title) = body.title else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let title = title.trim();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "Title required").into_response();
    }

    match sqlx::query("UPDATE tasks SET title = ?1, updated_at = datetime('now') WHERE id = ?2")
        .bind(title)
        .bind(id)
        .execute(&pool)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to update task");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to update task").into_response()
        }
    }
}

async fn toggle_collapse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    let task = match require_task(&pool, id).await {
        Ok(task) => task,
        Err(response) => return response,
    };
    if task.parent_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "Only parent tasks can be collapsed",
        )
            .into_response();
    }

    if let Err(err) = sqlx::query(
        "UPDATE tasks SET collapsed = 1 - collapsed, updated_at = datetime('now') WHERE id = ?1",
    )
    .bind(id)
    .execute(&pool)
    .await
    {
        tracing::error!(?err, id, "failed to collapse task");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to update task").into_response();
    }

    match fetch_task_with_children(&pool, id).await {
        Ok(Some(task)) => Html(render_task(&task)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Task not found").into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to reload task");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reload task").into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReorderBody {
    bucket: Option<String>,
    parent_id: Option<i64>,
    ordered_ids: Vec<i64>,
}

async fn reorder_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReorderBody>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    if body.parent_id.is_none() && body.bucket.as_deref().and_then(parse_bucket).is_none() {
        return (StatusCode::BAD_REQUEST, "Invalid reorder target").into_response();
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(?err, "failed to begin reorder transaction");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reorder tasks").into_response();
        }
    };
    for (idx, id) in body.ordered_ids.iter().enumerate() {
        if let Err(err) = sqlx::query(
            "UPDATE tasks SET position = ?1, updated_at = datetime('now') WHERE id = ?2",
        )
        .bind((idx as i64) + 1)
        .bind(id)
        .execute(&mut *tx)
        .await
        {
            tracing::error!(?err, id, "failed to reorder task");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reorder tasks").into_response();
        }
    }
    if let Err(err) = tx.commit().await {
        tracing::error!(?err, "failed to commit reorder transaction");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reorder tasks").into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveBody {
    id: i64,
    bucket: Option<String>,
    parent_id: Option<i64>,
    index: Option<usize>,
}

async fn move_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MoveBody>,
) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    if let Some(parent_id) = body.parent_id {
        move_task_to_parent(&pool, body.id, parent_id, body.index.unwrap_or(0)).await
    } else {
        let Some(bucket) = body.bucket.as_deref().and_then(parse_bucket) else {
            return (StatusCode::BAD_REQUEST, "Invalid bucket").into_response();
        };
        move_task_to_bucket(&pool, body.id, bucket, body.index.unwrap_or(0)).await
    }
}

async fn move_task_to_parent(pool: &SqlitePool, id: i64, parent_id: i64, index: usize) -> Response {
    if id == parent_id {
        return (StatusCode::BAD_REQUEST, "A task cannot be its own subtask").into_response();
    }
    let parent = match require_task(pool, parent_id).await {
        Ok(task) => task,
        Err(response) => return response,
    };
    if parent.parent_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "Subtasks can only be added to top-level tasks",
        )
            .into_response();
    }

    let child_ids = match sqlx::query_as::<_, (i64,)>(
        "SELECT id FROM tasks WHERE parent_id = ?1 ORDER BY position ASC, id ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows.into_iter().map(|row| row.0).collect::<Vec<_>>(),
        Err(err) => {
            tracing::error!(?err, id, "failed to load child tasks");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
        }
    };

    let position = (index as i64) + 1;
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(?err, id, parent_id, "failed to begin move transaction");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
        }
    };
    if let Err(err) = sqlx::query(
        r#"
        UPDATE tasks
        SET parent_id = ?1, bucket = ?2, task_type = ?3, position = ?4, collapsed = 0, updated_at = datetime('now')
        WHERE id = ?5
        "#,
    )
    .bind(parent.id)
    .bind(parent.bucket.as_str())
    .bind(parent.task_type.as_str())
    .bind(position)
    .bind(id)
    .execute(&mut *tx)
    .await
    {
        tracing::error!(?err, id, parent_id, "failed to move task under parent");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
    }
    for (offset, child_id) in child_ids.iter().enumerate() {
        if let Err(err) = sqlx::query(
            r#"
            UPDATE tasks
            SET parent_id = ?1, bucket = ?2, task_type = ?3, position = ?4, collapsed = 0, updated_at = datetime('now')
            WHERE id = ?5
            "#,
        )
        .bind(parent.id)
        .bind(parent.bucket.as_str())
        .bind(parent.task_type.as_str())
        .bind(position + offset as i64 + 1)
        .bind(child_id)
        .execute(&mut *tx)
        .await
        {
            tracing::error!(?err, id, child_id, parent_id, "failed to flatten child task under parent");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
        }
    }
    if let Err(err) =
        sqlx::query("UPDATE tasks SET collapsed = 0, updated_at = datetime('now') WHERE id = ?1")
            .bind(parent.id)
            .execute(&mut *tx)
            .await
    {
        tracing::error!(?err, parent_id, "failed to expand parent task");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
    }
    if let Err(err) = tx.commit().await {
        tracing::error!(?err, id, parent_id, "failed to commit move transaction");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
    }

    let mut moved_ids = Vec::with_capacity(child_ids.len() + 1);
    moved_ids.push(id);
    moved_ids.extend(child_ids);
    match render_moved_subtasks(pool, &moved_ids).await {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(?err, id, parent_id, "failed to reload moved subtasks");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reload tasks").into_response()
        }
    }
}

async fn render_moved_subtasks(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<String> {
    let mut html = String::new();
    for id in ids {
        if let Some(task) = fetch_task(pool, *id).await? {
            html.push_str(&render_subtask(&task));
        }
    }
    Ok(html)
}

async fn move_task_to_bucket(pool: &SqlitePool, id: i64, bucket: Bucket, index: usize) -> Response {
    let task = match require_task(pool, id).await {
        Ok(task) => task,
        Err(response) => return response,
    };
    let task_type = if matches!(bucket, Bucket::Today) {
        task.task_type
    } else {
        TaskType::from_bucket(bucket)
    };
    let collapsed = if task.parent_id.is_some() {
        true
    } else {
        task.collapsed
    };

    if let Err(err) = sqlx::query(
        r#"
        UPDATE tasks
        SET parent_id = NULL, bucket = ?1, task_type = ?2, position = ?3, collapsed = ?4, updated_at = datetime('now')
        WHERE id = ?5
        "#,
    )
    .bind(bucket.as_str())
    .bind(task_type.as_str())
    .bind((index as i64) + 1)
    .bind(if collapsed { 1_i64 } else { 0_i64 })
    .bind(id)
    .execute(pool)
    .await
    {
        tracing::error!(?err, id, "failed to move task to bucket");
        return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to move task").into_response();
    }

    match fetch_task_with_children(pool, id).await {
        Ok(Some(task)) => Html(render_task(&task)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Task not found").into_response(),
        Err(err) => {
            tracing::error!(?err, id, "failed to reload moved task");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to reload task").into_response()
        }
    }
}

async fn completed_tasks(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let pool = match require_pool(&state, &headers).await {
        Ok(pool) => pool,
        Err(response) => return response,
    };

    let rows = match sqlx::query(
        r#"
        SELECT id, title, task_type, bucket, completed, position, parent_id, collapsed, created_at, updated_at
        FROM tasks
        ORDER BY parent_id IS NOT NULL, parent_id, updated_at DESC, position ASC
        "#,
    )
    .fetch_all(&pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(?err, "failed to load completed tasks");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Unable to load completed tasks")
                .into_response();
        }
    };

    let mut top_level = Vec::new();
    let mut children: HashMap<i64, Vec<Task>> = HashMap::new();
    for row in rows {
        let task = task_from_row(&row);
        if let Some(parent_id) = task.parent_id {
            children.entry(parent_id).or_default().push(task);
        } else if task.completed {
            top_level.push(task);
        }
    }

    let mut html = String::from(
        "<div class='completed-tasklist-header'><span>Task</span><span>Completed</span><span></span></div><ul class='completed-tasklist'>",
    );
    for mut task in top_level.into_iter().take(100) {
        task.children = children.remove(&task.id).unwrap_or_default();
        html.push_str(&render_completed_task(&task));
    }
    html.push_str("</ul>");

    Html(html).into_response()
}

fn render_completed_task(task: &Task) -> String {
    let completed_time = task.updated_at.replace('T', " ");
    let completed_time = completed_time
        .split('.')
        .next()
        .unwrap_or(completed_time.as_str());
    let (date_part, time_part) = completed_time
        .split_once(' ')
        .unwrap_or((completed_time, ""));
    let mut children = String::new();
    if !task.children.is_empty() {
        children.push_str("<ul class='completed-subtasks'>");
        for child in &task.children {
            let child_class = if child.completed {
                "completed-subtask done"
            } else {
                "completed-subtask"
            };
            children.push_str(&format!(
                "<li class='{child_class}'>{}</li>",
                html_escape(&child.title)
            ));
        }
        children.push_str("</ul>");
    }

    format!(
        r#"<li class="completed-task" data-id="{id}">
    <div class="completed-task-main">
        <span class="completed-title">{title}</span>
        {children}
    </div>
    <span class="completed-time"><span class="completed-date">{date_part}</span><span class="completed-time-only">{time_part}</span></span>
    <span class="button-group">
        <button class="undo-btn" type="button" tabindex="-1" hx-post="/tasks/{id}/toggle" hx-target="closest li.completed-task" hx-swap="outerHTML" hx-on::afterRequest="window.location.reload()" title="Undo">&#8634;</button>
        <button class="delete-btn" type="button" tabindex="-1" hx-post="/tasks/{id}/delete" hx-target="closest li.completed-task" hx-swap="outerHTML" title="Delete">&times;</button>
    </span>
</li>"#,
        id = task.id,
        title = html_escape(&task.title),
    )
}
