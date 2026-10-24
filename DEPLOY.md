# Deploying to a low-memory NAS

Notes from deploying to a Synology DS220j (RTD1296, ARM64, 512 MB RAM). The NAS
cannot compile Rust — it does not have the memory — so the image is built on a
workstation and copied over.

## Why the image is built elsewhere

`Dockerfile` runs the Rust compiler on the **build** machine's own architecture
and cross-compiles to the target (`FROM --platform=$BUILDPLATFORM`). Building an
ARM64 image on an x86 workstation therefore runs at full native speed instead of
under QEMU emulation.

Two things matter for the binary to actually run on the NAS:

- **No `-C target-cpu=native`.** It tunes the binary for whichever machine ran
  the build. On a cross build that is the wrong CPU entirely, and the result can
  fault with `SIGILL` on the target.
- **Matching glibc.** The builder and the runtime stage are both Debian
  bookworm, so the binary is linked against the same glibc it will find at
  runtime. A newer builder base than runtime base produces a binary that will
  not start.

## Build

```
docker buildx create --name epbuilder --driver docker-container --bootstrap   # once
docker buildx build --builder epbuilder \
  --platform linux/arm64 \
  --tag eisenpower-rs:0.1.0 \
  --output type=docker,dest=eisenpower-rs-arm64.tar \
  .
gzip -9 eisenpower-rs-arm64.tar
```

The default `docker` buildx driver cannot write a tarball, which is why the
`docker-container` builder is created first.

## Copy and load

```
scp eisenpower-rs-arm64.tar.gz administrator@swenbackup.local:/volume1/docker/eisenpower-rs/
ssh administrator@swenbackup.local \
  'gunzip -c /volume1/docker/eisenpower-rs/eisenpower-rs-arm64.tar.gz | sudo /usr/local/bin/docker load'
```

Synology's Container Manager keeps `/var/run/docker.sock` owned by `root`, and
`/usr/local/bin` is not on the SSH `PATH`. Either use full paths with `sudo`, or
grant passwordless access once:

```
echo "administrator ALL=(ALL) NOPASSWD: /usr/local/bin/docker, /usr/local/bin/docker-compose" \
  | sudo tee /etc/sudoers.d/99-docker-administrator
sudo chmod 440 /etc/sudoers.d/99-docker-administrator
```

## Run

`/volume1/docker/eisenpower-rs/` holds `docker-compose.yaml`, `.env`, and a
`data/` directory:

```
cd /volume1/docker/eisenpower-rs
mkdir -p data
chmod 775 data          # see below -- do not skip this
sudo /usr/local/bin/docker compose up -d
```

### The `chmod` is not optional

A new directory under `/volume1/docker` inherits the shared folder's Synology
ACL. `ls` on the NAS reports it as `drwxrwxrwx+`, but that mode is synthesised
from the ACL: the underlying POSIX mode is `555`, and a container process sees
only the POSIX mode. The app then fails on startup with

```
Error: creating auth directory /app/data/_auth
Caused by: Permission denied (os error 13)
```

`chmod 775 data`, run as the directory's owner (no `sudo` needed), sets a real
POSIX mode that the container honours. Running the container as root would also
work, which is why this is easy to miss.

Points worth keeping:

- **`user: "1026:100"`** runs the container as `administrator:users` so the
  SQLite files in `data/` stay readable over SMB and in File Station. Check your
  own IDs with `id administrator`.
- **`./data` is a bind mount**, not a named volume, so the databases sit in the
  shared folder where the NAS backup jobs can reach them.
- **`mem_limit: 256m`** keeps one runaway process from taking down a NAS that
  only has 512 MB in total. The app idles at roughly 15 MB; Argon2 briefly uses
  another 19 MB per sign-in.
- The `EISENPOWER_ADMIN_*` variables in `.env` seed the first administrator and
  are ignored afterwards, so changing that password in the UI is not undone by a
  restart.

## Upgrading

Build a new tag, load it, point the compose file at it, and recreate:

```
sudo /usr/local/bin/docker compose up -d --force-recreate
```

`data/` is untouched by this, so accounts and tasks survive. Old images can be
cleared with `sudo /usr/local/bin/docker image prune`.

## Backups

Everything that matters is in `/volume1/docker/eisenpower-rs/data/`:

- `_auth/users.db` — accounts and password hashes
- `_auth/session.key` — the HMAC key for sign-in cookies; losing it just signs
  everyone out
- `<username>.db` — one task database per user

Stop the container before copying them, or use `sqlite3 .backup`, so you do not
capture a database mid-write.
