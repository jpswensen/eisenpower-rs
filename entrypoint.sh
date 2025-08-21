#!/bin/sh
set -e

# Create the database and run all migration SQL files
if [ ! -f /app/tasks.db ]; then
  for f in /app/migrations/*.sql; do
    echo "Running migration: $f"
    sqlite3 /app/tasks.db < "$f"
  done
fi

exec /app/eisenpower-rs
