FROM rust:1.93-slim AS builder

ARG FEATURES=""

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev g++ make && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

# Build actual binary
COPY src/ src/
COPY migrations/ migrations/
COPY data/ data/
RUN touch src/main.rs src/lib.rs && \
    if [ -n "$FEATURES" ]; then \
      cargo build --release --features "$FEATURES"; \
    else \
      cargo build --release; \
    fi

# --- Runtime ---
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget && rm -rf /var/lib/apt/lists/*

RUN useradd -r -s /bin/false bloop
WORKDIR /app

COPY --from=builder /build/target/release/bloop /app/bloop
# Copy config but override hmac_secret via env var (never bundle secrets in images)
COPY config.toml /app/config.toml

RUN mkdir -p /data && chown bloop:bloop /data

USER bloop

ENV BLOOP__DATABASE__PATH=/data/bloop.db
ENV RUST_LOG=bloop=info
# REQUIRED: Set BLOOP__AUTH__HMAC_SECRET at runtime (min 32 chars)
# docker run -e BLOOP__AUTH__HMAC_SECRET=your-secret-here ...

EXPOSE 5332

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD wget -q --spider http://localhost:5332/health || exit 1

VOLUME ["/data"]

ENTRYPOINT ["/app/bloop"]
