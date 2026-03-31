# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
# Full CI check (run before pushing)
make ci-local

# Individual checks
cargo fmt --all                    # Format
cargo clippy --all-targets --all-features -- -D warnings  # Lint (run per crate: cd common && ..., cd control && ...)
cargo test --workspace             # All tests

# Run a single test
cargo test test_name -- --nocapture

# Quick compilation check (no codegen)
cargo check --all-targets --all-features

# Justfile alternatives
just ci                            # Full CI + audit + release build
just test-one TEST_NAME            # Single test with output
just watch                         # Live reload tests (requires cargo-watch)

# Kubernetes dev loop
just dev                           # Skaffold dev (auto-rebuild on change)
```

## Architecture

Rust workspace with two crates: `common` (shared types, Maglev algorithm) and `control` (main controller binary).

**Two subsystems in `control/src/`:**

- **`apis/gateway/`** — Kubernetes controllers (kube-rs reconcilers) that watch Gateway API resources (GatewayClass, Gateway, HTTPRoute, EndpointSlice, Secret) and configure the proxy layer.
- **`proxy/`** — L7 HTTP proxy: listener management, request routing (matchit radix tree), Maglev consistent hashing for backend selection, connection pooling (HTTP/1.1 + HTTP/2), TLS termination (rustls), filters, circuit breaker, rate limiter, health checker, and Prometheus metrics.

**Request flow:** Client → Listener → Router (matchit path match → Maglev hash → backend) → Filters → Forwarder (connection pool) → Backend

**Key shared state:** `Router` holds `RwLock<matchit::Router<RouteKey>>` for path matching and `RwLock<HashMap<RouteKey, Route>>` for route data including per-route Maglev lookup tables.

## Rust Rules (Mandatory)

1. **No `.unwrap()` in production** — Use `?`, `ok_or_else`, or the safe lock helpers
2. **No `println!`** — Use `tracing::{info, warn, error, debug}`
3. **No string enums** — Use proper Rust enums
4. **No TODOs or stubs** — Complete implementations only

### Safe Lock Helpers (Always use these instead of raw `.unwrap()` on locks)

```rust
// Use safe_read(&self.routes) and safe_write(&self.routes)
// They recover from RwLock poisoning instead of panicking
```

### Error Handling

```rust
// Use .ok_or_else() or ? — never .unwrap() on Option/Result
let name = gateway.metadata.name.as_ref()
    .ok_or_else(|| anyhow!("Gateway missing name"))?;
```

## TDD Workflow

**RED → GREEN → REFACTOR** — Always write a failing test first, then implement minimally, then clean up.

## Common Tasks

### Adding a New Filter Type
1. Add variant to `FilterAction` enum in `proxy/filters.rs`
2. Implement in `apply_request_filters()` or `apply_response_filters()`
3. Parse from HTTPRoute in `apis/gateway/http_route.rs`

### Adding a New Matching Condition
1. Add to `RouteMatch` struct in `proxy/router.rs`
2. Update `matches_request()`
3. Parse from HTTPRoute in `apis/gateway/http_route.rs`

### Adding a New Metric
1. Register in `proxy/metrics.rs`
2. Instrument in relevant code path

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RAUTA_K8S_MODE` | `false` | Enable Kubernetes mode |
| `RAUTA_BIND_ADDR` | `0.0.0.0:8080` | Listen address (standalone) |
| `RAUTA_BACKEND_ADDR` | — | Backend address (standalone) |
| `RAUTA_GATEWAY_CLASS` | `rauta` | GatewayClass name to watch |
| `RUST_LOG` | `info` | Log level |

## Project Context

This is a **learning project** exploring Kubernetes Gateway API, Rust async, and L7 proxy patterns. Code quality matters, but we're experimenting. Stage 1 (Gateway API controller + HTTP proxy) is complete. Stage 2 (WASM plugin system) is planned.
