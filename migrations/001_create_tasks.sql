-- 001_create_tasks.sql
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    task_type TEXT NOT NULL CHECK(task_type IN ('UrgentImportant','UrgentNotImportant','NotUrgentImportant','NotUrgentNotImportant')),
    bucket TEXT NOT NULL CHECK(bucket IN ('UrgentImportant','UrgentNotImportant','NotUrgentImportant','NotUrgentNotImportant','Today')),
    completed INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_tasks_bucket_position ON tasks(bucket, position);
