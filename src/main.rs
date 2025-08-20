use axum::{
    extract::{Form, Json, Path, State},
    http::{Request, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post, patch},
    Router,
    middleware::{self, Next},
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use sqlx::Row;
use std::{collections::BTreeMap, net::SocketAddr};
use tower_http::services::ServeDir;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;


#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Copy, Serialize)]
enum TaskType {
    UrgentImportant,
    UrgentNotImportant,
    NotUrgentImportant,
    NotUrgentNotImportant,
}

impl TaskType {
    fn from_bucket(b: Bucket) -> TaskType {
        match b {
            Bucket::UrgentImportant => TaskType::UrgentImportant,
            Bucket::UrgentNotImportant => TaskType::UrgentNotImportant,
            Bucket::NotUrgentImportant => TaskType::NotUrgentImportant,
            Bucket::NotUrgentNotImportant => TaskType::NotUrgentNotImportant,
            Bucket::Today => TaskType::UrgentImportant, // default when adding directly to Today
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

#[derive(Debug, Clone, Copy, Serialize)]
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
    task_type: TaskType, // color source for 'Today'
    bucket: Bucket,      // actual column the task is in
    completed: bool,
    position: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).ok();

    let db_url = "sqlite://tasks.db";
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await?;

    // Run migrations from ./migrations
    sqlx::migrate!("./migrations").run(&pool).await?;

    let state = AppState { pool };

    // Read port from env
    let port: u16 = std::env::var("PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8080);

    let app = Router::new()
        .route("/", get(index))
        .route("/tasks", post(add_task))
        .route("/tasks/{id}/delete", post(delete_task))
        .route("/tasks/{id}/toggle", post(toggle_task))
        .route("/tasks/{id}", patch(update_task))
        .route("/reorder", post(reorder_bucket))
        .route("/move", post(move_task))
        .route("/completed", get(completed_tasks)) // Route for completed tasks
        .with_state(state.clone())
        .nest_service("/static", ServeDir::new("static"))
        .layer(middleware::from_fn(basic_auth));

    use tokio::net::TcpListener;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    info!(?addr, "listening");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let groups = fetch_all_grouped(&state.pool).await.unwrap_or_default();
    let html = render_index(groups);
    Html(html)
}

fn render_index(groups: BTreeMap<&'static str, Vec<Task>>) -> String {
    let ui = groups.get("UrgentImportant").cloned().unwrap_or_default().into_iter().filter(|t| !t.completed).collect::<Vec<_>>();
    let uni = groups.get("UrgentNotImportant").cloned().unwrap_or_default().into_iter().filter(|t| !t.completed).collect::<Vec<_>>();
    let nui = groups.get("NotUrgentImportant").cloned().unwrap_or_default().into_iter().filter(|t| !t.completed).collect::<Vec<_>>();
    let nun = groups.get("NotUrgentNotImportant").cloned().unwrap_or_default().into_iter().filter(|t| !t.completed).collect::<Vec<_>>();
    let today = groups.get("Today").cloned().unwrap_or_default().into_iter().filter(|t| !t.completed).collect::<Vec<_>>();

            let s = format!(r#"<!DOCTYPE html>
    <html lang="en">
    <head>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
    <title>Eisenhower Matrix</title>
    <link rel="stylesheet" href="/static/style.css">
    <script src="https://unpkg.com/htmx.org@1.9.10"></script>
    <script src="https://cdn.jsdelivr.net/npm/sortablejs@1.15.2/Sortable.min.js"></script>
    </head>
    <body>
    <div class="matrix-scroll">
        <div class="header">
        <strong>Eisenhower Matrix</strong>
        <span class="small muted">Rust + Axum + SQLx • SQLite file: tasks.db</span>
    <button id="refresh-btn" style="float:right; margin-left:8px;">Refresh</button>
    <button id="show-completed-btn" style="float:right; margin-left:16px;">Completed Tasks</button>
    </div>
    <div class="grid">
        <section class="column ui">
                <div class="column-title"><div>Urgent & Important</div><span class="badge">Add / Drag</span></div>
                {}
        </section>
        <section class="column uni">
                <div class="column-title"><div>Urgent & Not Important</div><span class="badge">Add / Drag</span></div>
                {}
        </section>
        <section class="column today">
                <div class="column-title"><div>Today's Tasks</div><span class="badge">Drag from any column</span></div>
                {}
        </section>
        <section class="column nui">
                <div class="column-title"><div>Not Urgent & Important</div><span class="badge">Add / Drag</span></div>
                {}
        </section>
        <section class="column nun">
                <div class="column-title"><div>Not Urgent & Not Important</div><span class="badge">Add / Drag</span></div>
                {}
        </section>
    </div>
    <div id="completed-panel" class="completed-panel" style="display:none;">
        <div class="completed-panel-content">
            <button id="close-completed-btn" style="float:right;">Close</button>
            <h2>Completed Tasks</h2>
            <div id="completed-tasks-list" hx-get="/completed" hx-trigger="revealed" hx-swap="innerHTML"></div>
        </div>
    </div>
</div>
<script>
    function bootSortable(listId, bucket){{
        const el = document.getElementById(listId);
        if(!el) return;
        new Sortable(el, {{
            animation: 150,
            group: 'matrix',
            delay: 350,
            delayOnTouchOnly: true,
            onEnd: function(evt){{
                const ids = Array.from(el.querySelectorAll('li.task')).map(li => Number(li.dataset.id));
                fetch('/reorder', {{
                    method: 'POST',
                    headers: {{'Content-Type':'application/json'}},
                    body: JSON.stringify({{ bucket: bucket, orderedIds: ids }})
                    }}).then(() => {{
                    if(evt.item && evt.to){{
                        const newBucket = evt.to.dataset.bucket;
                        fetch('/move', {{
                            method:'POST',
                            headers:{{'Content-Type':'application/json'}},
                            body: JSON.stringify({{ id: Number(evt.item.dataset.id), bucket: newBucket, index: evt.newIndex }})
                        }})
                        .then(response => response.text())
                        .then(html => {{
                            // Replace the moved element's HTML with the updated HTML from the server
                            // and re-initialize HTMX on the new element
                            const temp = document.createElement('div');
                            temp.innerHTML = html;
                            const newElem = temp.firstElementChild;
                            evt.item.replaceWith(newElem);
                            if(window.htmx && newElem) {{
                                window.htmx.process(newElem);
                            }}
    }});
}}
}});
}}
}});
}}
        document.addEventListener('DOMContentLoaded', function(){{
                bootSortable('list-UI', 'UrgentImportant');
                bootSortable('list-UNI', 'UrgentNotImportant');
                bootSortable('list-NUI', 'NotUrgentImportant');
                bootSortable('list-NUN', 'NotUrgentNotImportant');
                bootSortable('list-TODAY', 'Today');
}});

        // Preserve horizontal scroll position across HTMX reloads
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
            document.getElementById('completed-panel').style.display = 'block';
            document.getElementById('completed-tasks-list').dispatchEvent(new Event('revealed'));
}};
            document.getElementById('refresh-btn').onclick = function() {{
                const ms = document.querySelector('.matrix-scroll');
                if (ms) {{
                    sessionStorage.setItem('matrixScrollX', ms.scrollLeft);
                }}
                window.location.reload();
            }};
        document.getElementById('close-completed-btn').onclick = function() {{
            const ms = document.querySelector('.matrix-scroll');
            if (ms) {{
                sessionStorage.setItem('matrixScrollX', ms.scrollLeft);
}}
            document.getElementById('completed-panel').style.display = 'none';
            window.location.reload();
}};
        // Restore horizontal scroll position after reload
        document.addEventListener('DOMContentLoaded', function() {{
            const ms = document.querySelector('.matrix-scroll');
            const scrollX = sessionStorage.getItem('matrixScrollX');
            if (ms && scrollX) {{
                ms.scrollLeft = parseInt(scrollX, 10);
                sessionStorage.removeItem('matrixScrollX');
}}
}});
    // No custom event listeners needed; Undo button uses hx-on::afterRequest for reload
</script>
</body></html>
"#,
    render_column("UrgentImportant", "list-UI", &ui),
    render_column("UrgentNotImportant", "list-UNI", &uni),
    render_column("Today", "list-TODAY", &today),
    render_column("NotUrgentImportant", "list-NUI", &nui),
    render_column("NotUrgentNotImportant", "list-NUN", &nun),
);
    s
}

fn render_column(bucket: &str, list_id: &str, tasks: &Vec<Task>) -> String {
    let mut html = String::new();
    html.push_str(&format!(r#"<ul class="tasklist" id="{}" data-bucket="{}">"#, list_id, bucket));
    for t in tasks {
        html.push_str(&render_task(t));
    }
    html.push_str("</ul>");
    html.push_str(&format!(r#"
    <form class='add-form' hx-post='/tasks' hx-target='#{0}' hx-swap='beforeend' hx-on::after-request="this.reset()">
  <input type='hidden' name='bucket' value='{1}'/>
  <input type='text' name='title' placeholder='Add new task here...' autocomplete='off'>
  <button type='submit'>Add</button>
</form>
"#, list_id, bucket));
    html
}

fn render_task(t: &Task) -> String {
    let chip = match t.bucket {
        Bucket::Today => match t.task_type {
            TaskType::UrgentImportant => "color-UI",
            TaskType::UrgentNotImportant => "color-UNI",
            TaskType::NotUrgentImportant => "color-NUI",
            TaskType::NotUrgentNotImportant => "color-NUN",
        },
        Bucket::UrgentImportant => "color-UI",
        Bucket::UrgentNotImportant => "color-UNI",
        Bucket::NotUrgentImportant => "color-NUI",
        Bucket::NotUrgentNotImportant => "color-NUN",
    };
    let title = html_escape(&t.title);
    // Use icons for Done (check square) and Undo (circular arrow)
    let done_button = if t.completed {
        // Undo: SVG undo background
        format!("<button class='undo-btn' hx-post='/tasks/{}/toggle' hx-swap='outerHTML' hx-target='closest li.task' hx-on::afterSwap='window.location.reload()' title='Undo'><span class='svg-undo'></span></button>", t.id)
    } else {
        // Done: SVG checkmark background
        format!("<button class='done-btn' hx-post='/tasks/{}/toggle' hx-swap='outerHTML' hx-target='closest li.task' hx-on::afterSwap='window.location.reload()' title='Done'><span class='svg-check'></span></button>", t.id)
    };
    let delete_button = format!("<button class='delete-btn' hx-post='/tasks/{}/delete' hx-target='closest li.task' hx-swap='outerHTML' title='Delete'><span class='svg-x'></span></button>", t.id);
    format!(r#"<li class="task" data-id="{}">
        <div class="color-chip {}"></div>
        <div class="text" contenteditable="true"
                 onblur="fetch('/tasks/{}', {{method:'PATCH', headers:{{'Content-Type':'application/json'}}, body: JSON.stringify({{title:this.innerText}})}})">{}</div>
        <div class="controls">
        {}{}
        </div>
    </li>"#, t.id, chip, t.id, title, done_button, delete_button)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn fetch_all_grouped(pool: &SqlitePool) -> anyhow::Result<BTreeMap<&'static str, Vec<Task>>> {
    let rows = sqlx::query(
        r#"SELECT id, title, task_type, bucket,
                  completed, position, created_at, updated_at
           FROM tasks
           ORDER BY bucket, position ASC"#)
        .fetch_all(pool)
        .await?;

    use sqlx::Row;
    let mut map: BTreeMap<&'static str, Vec<Task>> = BTreeMap::new();
    for r in rows {
        let tp = match r.get::<String, _>("task_type").as_str() {
            "UrgentImportant" => TaskType::UrgentImportant,
            "UrgentNotImportant" => TaskType::UrgentNotImportant,
            "NotUrgentImportant" => TaskType::NotUrgentImportant,
            _ => TaskType::NotUrgentNotImportant,
        };
        let bucket = match r.get::<String, _>("bucket").as_str() {
            "UrgentImportant" => Bucket::UrgentImportant,
            "UrgentNotImportant" => Bucket::UrgentNotImportant,
            "NotUrgentImportant" => Bucket::NotUrgentImportant,
            "NotUrgentNotImportant" => Bucket::NotUrgentNotImportant,
            _ => Bucket::Today,
        };
        let task = Task {
            id: r.get("id"),
            title: r.get("title"),
            task_type: tp,
            bucket,
            completed: r.get::<i64, _>("completed") != 0,
            position: r.get("position"),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        map.entry(bucket.as_str()).or_default().push(task);
    }
    Ok(map)
}

#[derive(Deserialize)]
struct NewTask {
    title: String,
    bucket: String,
}

async fn add_task(
    State(state): State<AppState>,
    Form(form): Form<NewTask>,
) -> impl IntoResponse {
    if form.title.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "Title required").into_response();
    }
    let bucket = parse_bucket(&form.bucket).unwrap_or(Bucket::UrgentImportant);
    let task_type = if matches!(bucket, Bucket::Today) {
        TaskType::UrgentImportant
    } else {
        TaskType::from_bucket(bucket)
    };

    let max_pos: Option<(i64,)> = sqlx::query_as(
        r#"SELECT COALESCE(MAX(position), 0) FROM tasks WHERE bucket = ?1"#,
    )
    .bind(bucket.as_str())
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();

    let pos = max_pos.map(|t| t.0 + 1).unwrap_or(1);

    let id = sqlx::query(
        r#"INSERT INTO tasks(title, task_type, bucket, position) VALUES (?1, ?2, ?3, ?4)"#,
    )
    .bind(form.title.trim())
    .bind(task_type.as_str())
    .bind(bucket.as_str())
    .bind(pos)
    .execute(&state.pool)
    .await
    .unwrap()
    .last_insert_rowid();

    let task = Task {
        id,
        title: form.title.trim().to_string(),
        task_type,
        bucket,
        completed: false,
        position: pos,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    Html(render_task(&task)).into_response()
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

async fn delete_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    sqlx::query(r#"DELETE FROM tasks WHERE id = ?1"#)
        .bind(id)
        .execute(&state.pool)
        .await
        .ok();
    Html(String::new())
}

async fn toggle_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let _ = sqlx::query(
        r#"UPDATE tasks
           SET completed = 1 - completed, updated_at = datetime('now')
           WHERE id = ?1"#,
    )
    .bind(id)
    .execute(&state.pool)
    .await;
    // use sqlx::Row;
    if let Ok(Some(_r)) = sqlx::query(
        r#"SELECT id FROM tasks WHERE id = ?1"#,
    )
    .bind(id)
    .fetch_optional(&state.pool).await {
        // Always remove the <li> from the current list; JS will reload as needed
        return Html("").into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

#[derive(Deserialize)]
struct UpdateBody { title: Option<String> }
async fn update_task(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateBody>,
) -> impl IntoResponse {
    if let Some(title) = body.title {
        let _ = sqlx::query(r#"UPDATE tasks SET title = ?1, updated_at = datetime('now') WHERE id = ?2"#)
            .bind(title.trim())
            .bind(id)
            .execute(&state.pool).await;
        return StatusCode::NO_CONTENT.into_response();
    }
    StatusCode::BAD_REQUEST.into_response()
}

// --- PATCH: Add #[serde(rename_all = "camelCase")] to ensure JSON keys match JS ---
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReorderBody { bucket: String, ordered_ids: Vec<i64> }
async fn reorder_bucket(
    State(state): State<AppState>,
    Json(body): Json<ReorderBody>,
) -> impl IntoResponse {
    let _b = parse_bucket(&body.bucket).unwrap_or(Bucket::UrgentImportant);
    let mut tx = state.pool.begin().await.unwrap();
    for (idx, id) in body.ordered_ids.iter().enumerate() {
        let _ = sqlx::query(r#"UPDATE tasks SET position = ?1, updated_at = datetime('now') WHERE id = ?2"#)
            .bind((idx as i64) + 1)
            .bind(id)
            .execute(&mut *tx).await;
    }
    tx.commit().await.ok();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveBody { id: i64, bucket: String, index: Option<usize> }
async fn move_task(
    State(state): State<AppState>,
    Json(body): Json<MoveBody>,
) -> impl IntoResponse {
    let new_bucket = parse_bucket(&body.bucket).unwrap_or(Bucket::UrgentImportant);

    let new_task_type = match new_bucket {
        Bucket::UrgentImportant => Some(TaskType::UrgentImportant),
        Bucket::UrgentNotImportant => Some(TaskType::UrgentNotImportant),
        Bucket::NotUrgentImportant => Some(TaskType::NotUrgentImportant),
        Bucket::NotUrgentNotImportant => Some(TaskType::NotUrgentNotImportant),
        Bucket::Today => None,
    };

    let pos = body.index.unwrap_or(0) as i64 + 1;

    if let Some(tp) = new_task_type {
        let _ = sqlx::query(
            r#"UPDATE tasks SET bucket = ?1, task_type = ?2, position = ?3, updated_at = datetime('now') WHERE id = ?4"#,
        )
        .bind(new_bucket.as_str())
        .bind(tp.as_str())
        .bind(pos)
        .bind(body.id)
        .execute(&state.pool).await;
        // Fetch and return the updated task HTML for immediate UI update
        if let Ok(Some(r)) = sqlx::query(
            r#"SELECT id, title, task_type, bucket, completed, position, created_at, updated_at FROM tasks WHERE id = ?1"#,
        )
        .bind(body.id)
        .fetch_optional(&state.pool).await {
            let t = Task {
                id: r.get("id"),
                title: r.get("title"),
                task_type: match r.get::<String, _>("task_type").as_str() {
                    "UrgentImportant" => TaskType::UrgentImportant,
                    "UrgentNotImportant" => TaskType::UrgentNotImportant,
                    "NotUrgentImportant" => TaskType::NotUrgentImportant,
                    _ => TaskType::NotUrgentNotImportant,
                },
                bucket: parse_bucket(&r.get::<String, _>("bucket")).unwrap_or(Bucket::UrgentImportant),
                completed: r.get::<i64, _>("completed") != 0,
                position: r.get("position"),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            return Html(render_task(&t)).into_response();
        }
    } else {
        let _ = sqlx::query(
            r#"UPDATE tasks SET bucket = ?1, position = ?2, updated_at = datetime('now') WHERE id = ?3"#,
        )
        .bind(new_bucket.as_str())
        .bind(pos)
        .bind(body.id)
        .execute(&state.pool).await;
        if let Ok(Some(r)) = sqlx::query(
            r#"SELECT id, title, task_type, bucket, completed, position, created_at, updated_at FROM tasks WHERE id = ?1"#,
        )
        .bind(body.id)
        .fetch_optional(&state.pool).await {
            let t = Task {
                id: r.get("id"),
                title: r.get("title"),
                task_type: match r.get::<String, _>("task_type").as_str() {
                    "UrgentImportant" => TaskType::UrgentImportant,
                    "UrgentNotImportant" => TaskType::UrgentNotImportant,
                    "NotUrgentImportant" => TaskType::NotUrgentImportant,
                    _ => TaskType::NotUrgentNotImportant,
                },
                bucket: parse_bucket(&r.get::<String, _>("bucket")).unwrap_or(Bucket::UrgentImportant),
                completed: r.get::<i64, _>("completed") != 0,
                position: r.get("position"),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            return Html(render_task(&t)).into_response();
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

// Render completed tasks list for the panel
async fn completed_tasks(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query(
        r#"SELECT id, title, task_type, bucket, completed, position, created_at, updated_at FROM tasks WHERE completed = 1 ORDER BY updated_at DESC LIMIT 100"#
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    use sqlx::Row;
    let mut html = String::new();
    html.push_str("<div class='completed-tasklist-header'><span>Task</span><span>Completed</span><span></span><span></span></div>");
    html.push_str("<ul class='completed-tasklist'>");
        for r in rows {
            let id: i64 = r.get("id");
            let title: String = r.get("title");
            let updated_at: String = r.get::<String, _>("updated_at");
            // Format the completed time for display: date and time on separate lines for mobile
            let updated_at_fmt = updated_at.replace('T', " ");
            let completed_time = updated_at_fmt.split('.').next().unwrap_or(updated_at_fmt.as_str());
            let (date_part, time_part) = if let Some((d, t)) = completed_time.split_once(' ') {
                (d, t)
            } else {
                (completed_time, "")
            };
            html.push_str(&format!(
                "<li class='completed-task' data-id='{}'>\
                        <span class='completed-title'>{}</span>\
                        <span class='completed-time'><span class='completed-date'>{}</span><span class='completed-time-only'>{}</span></span>\
                        <span class='button-group'>\
                            <button class='undo-btn' hx-post='/tasks/{}/toggle' hx-target='closest li.completed-task' hx-swap='outerHTML' hx-on::afterSwap='document.dispatchEvent(new CustomEvent(\"completed-task-undone\"))' hx-on::afterRequest='window.location.reload()' title='Undo'><span class='svg-undo'></span></button>\
                            <button class='delete-btn' hx-post='/tasks/{}/delete' hx-target='closest li.completed-task' hx-swap='outerHTML' title='Delete'><span class='svg-x'></span></button>\
                        </span>\
                </li>",
                id, title, date_part, time_part, id, id
            ));
    }
    html.push_str("</ul>");
    Html(html)
}

async fn basic_auth(req: Request<axum::body::Body>, next: Next) -> Result<Response, StatusCode> {
    let env_user = std::env::var("EISENHOWER_USERNAME").unwrap_or_else(|_| "admin".to_string());
    let env_pass = std::env::var("EISENHOWER_PASSWORD").unwrap_or_else(|_| "password".to_string());
    if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(basic) = auth_str.strip_prefix("Basic ") {
                if let Ok(decoded) = STANDARD.decode(basic) {
                    if let Ok(decoded_str) = std::str::from_utf8(&decoded) {
                        let mut parts = decoded_str.splitn(2, ':');
                        let username = parts.next().unwrap_or("");
                        let password = parts.next().unwrap_or("");
                        if username == env_user && password == env_pass {
                            return Ok(next.run(req).await);
                        }
                    }
                }
            }
        }
    }
    let mut res = Response::new("Unauthorized".into());
    *res.status_mut() = StatusCode::UNAUTHORIZED;
    res.headers_mut().insert(header::WWW_AUTHENTICATE, "Basic realm=\"User Visible Realm\"".parse().unwrap());
    Ok(res)
}