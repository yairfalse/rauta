# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Is This

RAUTA ("iron" in Finnish) is an AI-native Kubernetes Gateway API controller with an L7 HTTP proxy, written in Rust. Part of the FALSE Systems tool family (AHTI, POLKU, KULTA, TAPIO).

## Build & Test Commands

```bash
cargo build -p control                                    # Debug build
cargo build --release -p control                          # Release build (LTO)
cargo test --workspace                                    # All 220+ tests
cargo test TEST_NAME -- --nocapture                       # Single test with output
cargo fmt --all -- --check                                # Check format
cargo clippy --all-targets --all-features -- -D warnings  # Lint (strict)
cargo bench -p control --bench hot_path                   # Run hot-path benchmarks
make ci-local                                             # Full CI locally
```

`just test-one TEST` runs a single test with output. `make check` does a fast compile check without codegen (common + control only). `make ci-local` also only checks common + control — use `cargo test --workspace` for the full 5-crate suite. Pre-commit and pre-push hooks run fmt, clippy, and tests automatically.

### Benchmarks

`control/benches/hot_path.rs` — custom harness (not criterion), three benchmarks:
1. Circuit breaker `allow_request` (AtomicU64 CAS)
2. Rate limiter `check_rate_limit` (AtomicU64 CAS-backed token bucket)
3. Router `select_backend` (route lookup + Maglev + ArcSwap health check)

### Oracle (ground truth tests against live gateway)

```bash
# Start RAUTA first, then:
cargo test --manifest-path eval/oracle/Cargo.toml -- --nocapture
```

The oracle is a standalone crate (NOT a workspace member). It connects to a live RAUTA instance and validates external behavior. 21 numbered test cases.

## Architecture

**Workspace crates:**
- `common` — no_std shared types (HttpMethod, Backend, Maglev, RouteKey)
- `control` — main binary: proxy + K8s controllers + admin server
- `agent-api` — GatewayQuery trait, snapshot types, diagnostics engine (7 rules)
- `mcp-server` — 11 MCP tool definitions for AI agent integration
- `rauta-cli` — CLI binary (`rauta`) + kubectl plugin (`kubectl-rauta`)

**NOT workspace members:** `eval/oracle/` (standalone test binary)

**Crate dependencies:** `mcp-server` → `agent-api` (transport-agnostic tools). `rauta-cli` → `mcp-server` + `agent-api` (CLI + MCP dual interface). `control` is the main binary and does not depend on rauta-cli.

**Three subsystems in `control/src/`:**

1. **`apis/gateway/`** — K8s controllers (kube-rs reconcilers). Watch GatewayClass, Gateway, HTTPRoute, EndpointSlice, Secret. Push routing config into Router.

2. **`proxy/`** — HTTP proxy engine. Request flow: Listener → Router → Filters → Forwarder → Backend.

3. **`admin/`** — Admin server on port 9091 (separate from data plane). REST API + LocalGatewayQuery reading live gateway state.

**Key data flow:** K8s reconcilers → `Router` (via `Arc<Router>`) → proxy server. Admin server reads from the same `Arc<Router>`, `Arc<CircuitBreakerManager>`, `Arc<RateLimiter>`.

### Lock-Free Hot Path

The proxy hot path uses atomics instead of locks for performance-critical state:

- **CircuitBreaker**: All state packed into single `AtomicU64` with CAS loops. Bit layout: `[63:62] state`, `[47:32] failures`, `[31:16] successes`, `[15:0] half_open_reqs`. Separate `AtomicU64` for last failure timestamp (microseconds).
- **TokenBucket**: Tokens (16.16 fixed-point) + timestamp packed into `AtomicU64`. CAS-based `try_acquire()`.
- **Health data**: `ArcSwap<HealthData>` for backend health + draining state. Single atomic load on hot path.
- **CircuitBreakerManager/RateLimiter**: `ArcSwap<HashMap>` for lock-free reads. `Mutex` only for new entry creation (never hot path).

### Error Handling

Proxy errors use `ProxyError` enum (in `error.rs`): `Timeout` → 504, `BackendError` → 502, `BodyTooLarge` → 413, `FilterError` → 500. `From<String>` impl provides backward compat for legacy sites (transitional).

## Rust Rules (Enforced)

1. **No `.unwrap()` in production code** — Use `?`, `safe_read()`/`safe_write()`, or `.ok_or_else()`. Tests may use `.unwrap()`.
2. **No `println!`** — Use `tracing::{info, warn, error, debug}`.
3. **No string enums** — Use proper Rust enums with `#[repr(u8)]` where appropriate.
4. **No new TODOs or stubs** — Complete implementations only. Existing incomplete operator surfaces must be made explicit in docs and errors until implemented.
5. **Safe lock helpers** for `RwLock`/`Mutex` — use `safe_read(&lock)` / `safe_write(&lock)` instead of `.read().unwrap()`. These recover from lock poisoning. Defined in `router.rs`.
6. **Clippy lints** in `control/Cargo.toml` warn on `unwrap_used`, `expect_used`, `panic`.
7. **Arc-wrap filters** in Route struct — `RouteMatch` construction uses `Arc::clone` (~1ns), not deep clone.
8. **Body size limit** — `http_body_util::Limited` enforces 10MB during streaming (not post-collect). `BodyTooLarge` errors don't count against backend health.

## TDD Workflow

RED → GREEN → REFACTOR. Write a failing test first, implement minimally, then clean up. Tests use `#[tokio::test]` for async.

## Common Extension Points

**Adding a filter:** Add variant to `FilterAction` in `filters.rs` → implement in `apply_request_filters()`/`apply_response_filters()` → parse from HTTPRoute in `http_route.rs` → add tests.

**Adding a diagnostic rule:** Implement `DiagnosticRule` trait in `agent-api/src/diagnostics/rules.rs` → register in `DiagnosticsEngine::with_builtin_rules()` → add test in `engine.rs`.

Diagnostic rules must preserve human-readable `evidence` and also attach `ontology_evidence` from `agent-api/src/ontology.rs`. Keep ontology schema changes backward-compatible or update README/agent docs with migration notes.

**Adding an MCP tool:** Add a `#[tool]` method and parameter type in `mcp-server/src/handler.rs` → add or reuse a method on the `GatewayQuery` trait in `agent-api/src/query.rs` → implement both `LocalGatewayQuery` in `control/src/admin/local_query.rs` and `RemoteGatewayQuery` in `rauta-cli/src/remote_query.rs` → add CLI/admin endpoints if the tool must work out of process.

**Adding a metric:** Register in `metrics.rs` → instrument in code path → test.

**Adding ontology or timeline behavior:** Use `agent-api/src/ontology.rs` for stable agent-facing entities and evidence. Keep eBPF as an optional evidence source that feeds ontology evidence, not as a required routing dependency.

## MCP Server (AI Agent Integration)

The MCP server lives in `rauta-cli`, not `control`. Start it with:

```bash
rauta --endpoint http://localhost:9091 mcp
```

This runs stdio transport: MCP JSON-RPC frames on stdout, tracing logs on stderr. Used by Claude Code, Cursor, and other MCP clients. The `RemoteGatewayQuery` (HTTP client to admin API) is wrapped in `RautaMcpHandler` and served over `rmcp::transport::stdio()`. Streamable HTTP transport (`POST /mcp` on admin port) is planned but not yet implemented.

Current remote read tools are wired through the admin API. Backend drain and undrain are intentionally explicit-unavailable operations until the safe-actions spec adds bounded action semantics.

## Environment Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `RAUTA_K8S_MODE` | `false` | Start K8s controllers |
| `RAUTA_BIND_ADDR` | `0.0.0.0:8080` | Proxy listen address |
| `RAUTA_BACKEND_ADDR` | — | Backend (standalone mode) |
| `RAUTA_ADMIN_PORT` | `9091` | Admin server port |
| `RAUTA_ADMIN_ENDPOINT` | `http://localhost:9091` | CLI target |
| `RAUTA_TLS_CERT` / `_KEY` | — | TLS termination |
| `RAUTA_GATEWAY_CLASS` | `rauta` | GatewayClass to watch |
| `RAUTA_TLS_HOSTNAME` | `localhost` | SNI hostname for TLS |
| `RUST_LOG` | `info` | Log level |

## Verification Before Commit

```bash
make ci-local   # runs fmt check, clippy, cargo check, tests
```

Pre-commit hooks enforce this automatically (blocks `.unwrap()`, `.expect()`, `panic!()` in non-test code, plus fmt and clippy). Pre-push hooks also run release build. Install hooks: `./scripts/git-hooks/install.sh`.

## Useful Scripts

- `deploy/deploy-to-kind.sh` — one-command Kind cluster + RAUTA + demo backend
- `scripts/run_load_test.sh` — load test suite with HTTP/2 backend
- `scripts/run-conformance.sh` — Gateway API conformance tests
- `scripts/watch_control.sh` — auto-rebuild on file changes
