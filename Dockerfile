# syntax=docker/dockerfile:1

# The compiler runs on the build machine's own architecture and cross-compiles
# to the target, so an ARM64 image can be produced on an x86 host at full speed
# rather than under QEMU emulation.
FROM --platform=$BUILDPLATFORM rust:1.89-bookworm AS builder

ARG TARGETARCH
WORKDIR /app

# libsqlite3-sys compiles SQLite from source, so a cross build needs a linker,
# a C compiler, and the target's libc headers. crossbuild-essential pulls in all
# three; gcc-aarch64-linux-gnu alone omits the headers and the compiler silently
# falls back to the host's, which then fails to resolve bits/libc-header-start.h.
RUN set -eux; \
    case "$TARGETARCH" in \
      arm64) target=aarch64-unknown-linux-gnu; packages=crossbuild-essential-arm64 ;; \
      amd64) target=x86_64-unknown-linux-gnu;  packages= ;; \
      *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$target" > /rust-target; \
    if [ -n "$packages" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends $packages; \
      rm -rf /var/lib/apt/lists/*; \
    fi; \
    rustup target add "$target"

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc

COPY Cargo.toml Cargo.lock ./
COPY src ./src

# No `-C target-cpu=native`: it would tune the binary for whichever machine ran
# the build, which is not the machine that runs the image.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    set -eux; \
    target="$(cat /rust-target)"; \
    cargo build --release --locked --target "$target"; \
    cp "target/$target/release/eisenpower-rs" /eisenpower-rs

# Bookworm on both sides keeps the runtime glibc identical to the one the
# binary was linked against.
FROM debian:bookworm-slim

RUN set -eux; \
    groupadd --system --gid 10001 eisenpower; \
    useradd --system --uid 10001 --gid 10001 --no-create-home eisenpower

WORKDIR /app
COPY --from=builder /eisenpower-rs /app/eisenpower-rs
COPY static ./static
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh /app/eisenpower-rs \
    && mkdir -p /app/data \
    && chown -R eisenpower:eisenpower /app

ENV EISENPOWER_DATA_DIR=/app/data \
    PORT=8080

EXPOSE 8080
VOLUME ["/app/data"]
USER eisenpower
ENTRYPOINT ["/entrypoint.sh"]
