# Build stage
FROM rust:1.89 as builder
WORKDIR /app
COPY . .
RUN RUSTFLAGS="-C target-cpu=native" cargo build --release

FROM debian:trixie-slim
WORKDIR /app
COPY --from=builder /app/target/release/eisenpower-rs /app/
COPY static ./static
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV EISENPOWER_USERS=admin:password
ENV EISENPOWER_DATA_DIR=/app/data
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/entrypoint.sh"]
