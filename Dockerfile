# Simple multi-stage Rust build for RAUTA
# Works on Mac (will use QEMU for linux/amd64)

FROM --platform=linux/amd64 rust:1.82-slim AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Copy workspace
COPY Cargo.toml Cargo.lock ./
COPY common ./common
COPY control ./control

# Build release
RUN cargo build --release --bin control

# Runtime image
FROM --platform=linux/amd64 debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy binary
COPY --from=builder /app/target/release/control /usr/local/bin/rauta

# Non-root user
RUN useradd -r -u 1000 rauta
USER rauta

EXPOSE 9090

ENTRYPOINT ["/usr/local/bin/rauta"]
