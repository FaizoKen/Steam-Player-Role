FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Cache dependencies in a separate layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/steam-player-role target/release/deps/steam_player_role*

# Build actual source
COPY src/ src/
COPY migrations/ migrations/
COPY favicon.ico ./
RUN cargo build --release && strip target/release/steam-player-role

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/steam-player-role /usr/local/bin/
EXPOSE 8088
CMD ["steam-player-role"]
