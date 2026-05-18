#!/bin/sh
set -e

mkdir -p "${EISENPOWER_DATA_DIR:-/app/data}"

exec /app/eisenpower-rs
