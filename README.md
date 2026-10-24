# EISENPOWER - Eisenhower Matrix with a current-day priority list : Rust + Axum + SQLite (+ optional Docker)

This is a modified form of the Eisenhower Matrix method for creating TODO lists according to urgency and importance. It also includes a Today's Tasks panel for ordering the tasks currently on the docket.

Several years ago I developed a version of this in Swift for MacOS and iOS, but that wasn't portable across devices, and wasn't web accessible. It also didn't track completed items and allow you to pull them out of the completed bin if you realized you weren't done yet.

Here are some of the features

- Drag & drop between columns to reorder/move.
- When dragging to the Today's Task list, it preserves which category it came from, in case you want to move it back to where it came from.
- Add tasks via the input at the bottom of each column.
- Click a task's text to edit. Automatically saved when navigating away.
- Create subtasks from any top-level task, collapse/expand them, and mark individual subtasks complete without hiding them.
- Drag a top-level task onto another task's subtask area to convert it into a subtask.
- Checkmark button to indicate the task is done and move it to the Completed Tasks list
- Completing a parent task completes all of its subtasks.
- X button to delete a task entirely.
- In the Completed Tasks panel, click the restore button to move the item back to the list of items still needing to be done.
- Sign in with your own account. Each user's tasks live in a separate SQLite file under the data directory.
- Administrators can add, remove, promote and reset users at `/admin` while the server is running.
- The database schema is initialized by the application at startup; migrations are intentionally not used while the app is still pre-release.

## Screenshots
![The main window](images/main_window.png)

![The completed tasks modal window](images/completed_tasks.png)


## How to Run

To run from the command line:

```
cargo run
```

On first start, with no accounts yet, an `admin` account is created with the
password `changeme-please`. Sign in at http://127.0.0.1:8080 and change it.
Pick your own instead with:

```
EISENPOWER_ADMIN_USERNAME='jpswensen' \
EISENPOWER_ADMIN_PASSWORD='replace-this-with-a-real-password' \
EISENPOWER_DATA_DIR='./data' \
cargo run
```

For an optimized local build:

```
cargo run --release
```

To run in Docker:

```
docker compose up --build
```

## Managing users

Accounts live in `<data dir>/_auth/users.db`, so they can be changed while the
server is running. Sign in as an administrator and open **Users** in the header
(or go to `/admin`) to:

- add a user, optionally as an administrator
- reset anyone's password
- grant or revoke administrator access
- remove a user, keeping or deleting their tasks

Passwords are stored as Argon2id hashes. Resetting a password or removing a
user invalidates that person's existing sign-in cookies immediately.

Two rules are enforced so nobody can lock everyone out: you cannot remove or
demote your own account, and the last remaining administrator cannot be removed
or demoted.

## Configuration

Everything is optional. The defaults run the app on port 8080 with a `data`
directory beside the binary.

| Variable | Default | Purpose |
| --- | --- | --- |
| `PORT` | `8080` | Port to listen on. |
| `EISENPOWER_DATA_DIR` | `data` | Where per-user task databases and the accounts database are stored. |
| `EISENPOWER_ADMIN_USERNAME` | `admin` | Administrator to create when no administrator exists yet. |
| `EISENPOWER_ADMIN_PASSWORD` | `changeme-please` | That administrator's initial password. |
| `EISENPOWER_USERS` | — | Optional `username:password` list used to seed accounts on first run. |
| `EISENPOWER_ADMINS` | — | Comma-separated usernames from `EISENPOWER_USERS` to make administrators. |
| `EISENPOWER_SESSION_SECRET` | generated | HMAC key for session cookies. Leave unset and a random one is generated and saved to `<data dir>/_auth/session.key`. |
| `EISENPOWER_SESSION_MAX_AGE` | `2592000` | Sign-in lifetime in seconds (30 days). |
| `EISENPOWER_COOKIE_SECURE` | `false` | Set to `true` when serving over HTTPS. |

`EISENPOWER_USERS` and the two `ADMIN` variables only ever *create* accounts
that do not exist yet. They never overwrite an account, so a password changed
in the admin UI survives a restart even if the old value is still in your
compose file.

## Deploying a prebuilt image

`Dockerfile` cross-compiles, so an image for a low-memory ARM64 box can be built
on a workstation instead of on the device itself:

```
docker buildx build --platform linux/arm64 --tag eisenpower-rs:0.1.0 \
  --output type=docker,dest=eisenpower-rs-arm64.tar .
```

See [DEPLOY.md](DEPLOY.md) for the full walkthrough used for a Synology NAS.

Enjoy!
