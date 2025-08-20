# Build stage
FROM rust:1.89 as builder
WORKDIR /app
COPY . .
RUN RUSTFLAGS="-C target-cpu=native" cargo build --release

FROM debian:trixie-slim
WORKDIR /app
RUN mkdir -p /var/cache/apt/archives/partial \
	&& apt-get update \
	&& apt-get install -y sqlite3 \
	&& rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/eisenhower_matrix /app/
COPY static ./static
COPY migrations ./migrations
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV EISENHOWER_USERNAME=admin
ENV EISENHOWER_PASSWORD=password
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/entrypoint.sh"]
