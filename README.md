# Eisenhower Matrix (Rust + Axum + SQLite)

Run:
```
cargo run
```
Then open http://127.0.0.1:8080

- Drag & drop between columns to reorder/move.
- Add tasks via the input at the bottom of each column.
- Click a task's text to edit; blur to save.
- Done/Undo toggles completion.
- ✕ deletes.
- Data lives in a local file `tasks.db` (created automatically).
