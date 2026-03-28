# =============================================================================
# Stage 1: deps — pre-build dependencies for caching
# =============================================================================
FROM rust:1.91-slim-bookworm AS deps

WORKDIR /build

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./

# Build a stub binary to cache all dependency compilation
RUN mkdir src && echo "fn main(){}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# =============================================================================
# Stage 2: dev — full toolchain, source mounted at runtime via volume
#   Used by docker compose for: cargo build, cargo test, cargo check, etc.
# =============================================================================
FROM deps AS dev

# Nothing extra — source is mounted as a volume at /build in compose

# =============================================================================
# Stage 3: build — compiles the actual binary
# =============================================================================
FROM deps AS builder

COPY src ./src
RUN touch src/main.rs && cargo build --release

# =============================================================================
# Stage 4: runtime — minimal production image
# =============================================================================
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
RUN mkdir -p /app/data

COPY --from=builder /build/target/release/polymarket_bot .

ENTRYPOINT ["./polymarket_bot"]
