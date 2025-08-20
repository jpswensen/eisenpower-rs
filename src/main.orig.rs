
use axum::{
    extract::{Form, Json, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post, patch},
    Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
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

    let app = Router::new()
        .route("/", get(index))
        .route("/tasks", post(add_task))
        .route("/tasks/:id/delete", post(delete_task))
        .route("/tasks/:id/toggle", post(toggle_task))
        .route("/tasks/:id", patch(update_task))
        .route("/reorder", post(reorder_bucket))
        .route("/move", post(move_task))
        .with_state(state.clone())
        .nest_service("/static", ServeDir::new("static"));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    info!(?addr, "listening");
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let groups = fetch_all_grouped(&state.pool).await.unwrap_or_default();
    let html = render_index(groups);
    Html(html)
}

fn render_index(groups: BTreeMap<&'static str, Vec<Task>>) -> String {
    let ui = groups.get("UrgentImportant").cloned().unwrap_or_default();
    let uni = groups.get("UrgentNotImportant").cloned().unwrap_or_default();
    let nui = groups.get("NotUrgentImportant").cloned().unwrap_or_default();
    let nun = groups.get("NotUrgentNotImportant").cloned().unwrap_or_default();
    let today = groups.get("Today").cloned().unwrap_or_default();

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
<div class="header">
<strong>Eisenhower Matrix</strong>
<span class="small muted">Rust + Axum + SQLx • SQLite file: tasks.db</span>
</div>
<div class="grid">
<section class="column ui">
  <div class="column-title"><div>Urgent & Important</div><span class="badge">Add / Drag</span></div>
  {{}} 
</section>
<section class="column uni">
  <div class="column-title"><div>Urgent & Not Important</div><span class="badge">Add / Drag</span></div>
  {{}}
</section>
<section class="column today">
  <div class="column-title"><div>Today's Tasks</div><span class="badge">Drag from any column</span></div>
  {{}}
</section>
<section class="column nui">
  <div class="column-title"><div>Not Urgent & Important</div><span class="badge">Add / Drag</span></div>
  {{}}
</section>
<section class="column nun">
  <div class="column-title"><div>Not Urgent & Not Important</div><span class="badge">Add / Drag</span></div>
  {{}}
</section>
</div>
<script>
  function bootSortable(listId, bucket){{
    const el = document.getElementById(listId);
    if(!el) return;
    new Sortable(el, {{
      animation: 150,
      group: 'matrix',
      onEnd: function(evt){{
        const ids = Array.from(el.querySelectorAll('li.task')).map(li => li.dataset.id);
        fetch('/reorder', {{
          method: 'POST',
          headers: {{'Content-Type':'application/json'}},
          body: JSON.stringify({{ bucket: bucket, ordered_ids: ids }})
        }}).then(() => {{
          if(evt.item && evt.to){{
            const newBucket = evt.to.dataset.bucket;
            fetch('/move', {{
              method:'POST',
              headers:{{'Content-Type':'application/json'}},
              body: JSON.stringify({{ id: evt.item.dataset.id, bucket: newBucket, index: evt.newIndex }})
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
    html.push_str(&format!(r#"<ul class="tasklist" id="{{}}" data-bucket="{{}}">"#, list_id, bucket));
    for t in tasks {
        html.push_str(&render_task(t));
    }
    html.push_str("</ul>");
    html.push_str(&format!(r#"
<form class="add-form" hx-post="/tasks" hx-target="#{}" hx-swap="beforeend">
  <input type="hidden" name="bucket" value="{}"/>
  <input type="text" name="title" placeholder="Add new task here..." autocomplete="off">
  <button type="submit">Add</button>
</form>
"#, list_id, bucket));
    html
}

fn render_task(t: &Task) -> String {
    let chip = match t.task_type {
        TaskType::UrgentImportant => "color-UI",
        TaskType::UrgentNotImportant => "color-UNI",
        TaskType::NotUrgentImportant => "color-NUI",
        TaskType::NotUrgentNotImportant => "color-NUN",
    };
    let title = html_escape(&t.title);
    let done_label = if t.completed { "Undo" } else { "Done" };
    format!(r#"<li class="task" data-id="{{}}">
  <div class="color-chip {{}}"></div>
  <div class="text" contenteditable="true"
       onblur="fetch('/tasks/{{}}', {{{{method:'PATCH', headers:{{{{'Content-Type':'application/json'}}}}, body: JSON.stringify({{{{title:this.innerText}}}})}}}})">{{}}</div>
  <div class="controls">
    <button hx-post="/tasks/{{}}/toggle" hx-swap="outerHTML" hx-target="closest li.task">{{}}</button>
    <button hx-post="/tasks/{{}}/delete" hx-target="closest li.task" hx-swap="outerHTML swap:1s">✕</button>
  </div>
</li>"#, t.id, chip, t.id, title, t.id, done_label, t.id)
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

    let mut map: BTreeMap<&'static str, Vec<Task>> = BTreeMap::new();
    for r in rows {
        let tp = match r.task_type.as_str() {
            "UrgentImportant" => TaskType::UrgentImportant,
            "UrgentNotImportant" => TaskType::UrgentNotImportant,
            "NotUrgentImportant" => TaskType::NotUrgentImportant,
            _ => TaskType::NotUrgentNotImportant,
        };
        let bucket = match r.bucket.as_str() {
            "UrgentImportant" => Bucket::UrgentImportant,
            "UrgentNotImportant" => Bucket::UrgentNotImportant,
            "NotUrgentImportant" => Bucket::NotUrgentImportant,
            "NotUrgentNotImportant" => Bucket::NotUrgentNotImportant,
            _ => Bucket::Today,
        };
        let task = Task {
            id: r.id,
            title: r.title,
            task_type: tp,
            bucket,
            completed: r.completed != 0,
            position: r.position,
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
        form.title.trim(),
        task_type.as_str(),
        bucket.as_str(),
        pos
    )
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
    sqlx::query(r#"DELETE FROM tasks WHERE id = ?1"#, id)
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
        id
    )
    .execute(&state.pool)
    .await;
    if let Ok(Some(r)) = sqlx::query(
        r#"SELECT id, title, task_type, bucket,
                  completed, position, created_at, updated_at
           FROM tasks WHERE id = ?1"#, id
    ).fetch_optional(&state.pool).await {
        let t = Task {
            id: r.id,
            title: r.title,
            task_type: match r.task_type.as_str() {
                "UrgentImportant" => TaskType::UrgentImportant,
                "UrgentNotImportant" => TaskType::UrgentNotImportant,
                "NotUrgentImportant" => TaskType::NotUrgentImportant,
                _ => TaskType::NotUrgentNotImportant,
            },
            bucket: parse_bucket(&r.bucket).unwrap_or(Bucket::UrgentImportant),
            completed: r.completed != 0,
            position: r.position,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        return Html(render_task(&t)).into_response();
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
        let _ = sqlx::query(r#"UPDATE tasks SET title = ?1, updated_at = datetime('now') WHERE id = ?2"#, title.trim(), id)
            .execute(&state.pool).await;
        return StatusCode::NO_CONTENT.into_response();
    }
    StatusCode::BAD_REQUEST.into_response()
}

#[derive(Deserialize)]
struct ReorderBody { bucket: String, ordered_ids: Vec<i64> }
async fn reorder_bucket(
    State(state): State<AppState>,
    Json(body): Json<ReorderBody>,
) -> impl IntoResponse {
    let _b = parse_bucket(&body.bucket).unwrap_or(Bucket::UrgentImportant);
    let mut tx = state.pool.begin().await.unwrap();
    for (idx, id) in body.ordered_ids.iter().enumerate() {
        let _ = sqlx::query(r#"UPDATE tasks SET position = ?1, updated_at = datetime('now') WHERE id = ?2"#,
            (idx as i64) + 1, id)
            .execute(&mut *tx).await;
    }
    tx.commit().await.ok();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
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

    let pos = body.index.map(|i| (i as i64) + 1).unwrap_or(1);

    if let Some(tp) = new_task_type {
        let _ = sqlx::query(
            r#"UPDATE tasks SET bucket = ?1, task_type = ?2, position = ?3, updated_at = datetime('now') WHERE id = ?4"#,
            new_bucket.as_str(),
            tp.as_str(),
            pos,
            body.id
        ).execute(&state.pool).await;
    } else {
        let _ = sqlx::query(
            r#"UPDATE tasks SET bucket = ?1, position = ?2, updated_at = datetime('now') WHERE id = ?3"#,
            new_bucket.as_str(),
            pos,
            body.id
        ).execute(&state.pool).await;
    }

    StatusCode::NO_CONTENT
}
