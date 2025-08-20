// main.rs (runtime SQLx + fixed axum + HTML templates)

use axum::{
    extract::{Form, Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post, delete, patch},
    Router,
};
use serde::Deserialize;
use sqlx::{SqlitePool, Row};
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;

#[derive(Clone)]
struct AppState {
    pool: Arc<SqlitePool>,
}

#[derive(Debug, Deserialize)]
struct TaskForm {
    title: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = SqlitePool::connect("sqlite://tasks.db").await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            task_type TEXT,
            bucket TEXT,
            completed INTEGER DEFAULT 0,
            position INTEGER,
            created_at TEXT DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&pool)
    .await?;

    let state = AppState {
        pool: Arc::new(pool),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/tasks", post(add_task))
        .route("/tasks/:id", delete(delete_task))
        .with_state(state);

    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    println!("Listening on http://{}", listener.local_addr()?);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query("SELECT id, title, completed FROM tasks ORDER BY id ASC")
        .fetch_all(&*state.pool)
        .await;

    let mut html = String::from("<h1>Tasks</h1><ul>");
    if let Ok(rows) = rows {
        for row in rows {
            let id: i64 = row.get("id");
            let title: String = row.get("title");
            let completed: i64 = row.get("completed");
            html.push_str(&format!(
                r#"<li data-id="{id}">{title} [{}]</li>"#,
                if completed != 0 { "✓" } else { " " }
            ));
        }
    }
    html.push_str("</ul>");
    Html(html)
}

async fn add_task(
    State(state): State<AppState>,
    Form(form): Form<TaskForm>,
) -> impl IntoResponse {
    let title = form.title.trim().to_string();
    let _ = sqlx::query(
        r#"INSERT INTO tasks(title, task_type, bucket, position) VALUES (?1, 'default', 'Inbox', 0)"#,
    )
    .bind(title)
    .execute(&*state.pool)
    .await;

    StatusCode::CREATED
}

async fn delete_task(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let _ = sqlx::query("DELETE FROM tasks WHERE id = ?1")
        .bind(id)
        .execute(&*state.pool)
        .await;

    StatusCode::NO_CONTENT
}
