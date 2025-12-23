# RAUTA: Gateway API Controller

**A learning project for Kubernetes Gateway API with L7 HTTP proxy**

---

## PROJECT STATUS

**What's Implemented:**
- Gateway API v1 (GatewayClass, Gateway, HTTPRoute)
- HTTP/1.1 and HTTP/2 proxy (cleartext and TLS)
- TLS termination with SNI and ALPN
- Maglev consistent hashing load balancer
- Path, header, and query parameter matching
- Request/response filters (headers, redirects)
- Rate limiting (token bucket)
- Circuit breaker (passive health checking)
- Connection pooling (per-backend, per-protocol)
- Retries with exponential backoff
- EndpointSlice watcher (dynamic pod discovery)
- Prometheus metrics
- 201+ test cases

**Code Stats:**
- ~15,000 lines of Rust
- Workspace: `common` (shared types) + `control` (main controller)
- jemalloc for async workloads
- Safe lock helpers (poison recovery)

---

## ARCHITECTURE OVERVIEW

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         RAUTA Controller                                 │
│                                                                          │
│  main.rs                                                                 │
│  └── Bootstrap: K8s client, listeners, controllers, metrics server      │
│                                                                          │
│  apis/gateway/                                                           │
│  ├── gateway_class.rs      # GatewayClass reconciler                    │
│  ├── gateway.rs            # Gateway reconciler + ListenerManager       │
│  ├── http_route.rs         # HTTPRoute reconciler → Router              │
│  ├── endpointslice_watcher.rs  # Dynamic pod IP discovery              │
│  ├── secret_watcher.rs     # TLS certificate management                 │
│  └── gateway_index.rs      # O(1) Gateway lookup optimization           │
│                                                                          │
│  proxy/                                                                  │
│  ├── router.rs             # matchit radix tree + Maglev tables         │
│  ├── server.rs             # HTTP server setup                          │
│  ├── listener_manager.rs   # Dynamic listener creation                  │
│  ├── request_handler.rs    # Request routing logic                      │
│  ├── forwarder.rs          # Backend request forwarding                 │
│  ├── filters.rs            # HTTPRoute filter types                     │
│  ├── tls.rs                # TLS/HTTPS with rustls                      │
│  ├── backend_pool.rs       # Connection pooling abstraction             │
│  ├── http1_pool.rs         # HTTP/1.1 connection pool                   │
│  ├── circuit_breaker.rs    # Passive health (error rate threshold)      │
│  ├── rate_limiter.rs       # Token bucket algorithm                     │
│  ├── health_checker.rs     # Active health (TCP probes)                 │
│  ├── worker.rs             # Per-core workers (lock-free)               │
│  └── metrics.rs            # Prometheus /metrics                        │
│                                                                          │
│  common/                                                                 │
│  └── lib.rs                # HttpMethod, Backend, Maglev, RouteKey      │
└─────────────────────────────────────────────────────────────────────────┘
```

### Request Flow

```
Client → Listener → Router → Filters → Forwarder → Backend
                      │
                      └── matchit (path) → Maglev (backend selection)
```

### Key Design Decisions

1. **Userspace L7** - HTTP/2 and TLS require TCP reassembly (XDP can't do this)
2. **Maglev hashing** - O(1) lookup, ~1/N disruption on backend changes
3. **Gateway API** - Modern K8s standard, not legacy Ingress
4. **Safe lock helpers** - Recover from RwLock poisoning instead of cascading panics

---

## RUST REQUIREMENTS

### Absolute Rules

1. **No `.unwrap()` in production** - Use `?` or `safe_read()`/`safe_write()`
2. **No `println!`** - Use `tracing::{info, warn, error, debug}`
3. **No string enums** - Use proper Rust enums
4. **No TODOs or stubs** - Complete implementations only

### Safe Lock Helpers (MANDATORY)

```rust
// BAD - Will panic on lock poisoning
let routes = self.routes.read().unwrap();

// GOOD - Recovers from poisoning
let routes = safe_read(&self.routes);

// The helper:
fn safe_read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| {
        warn!("RwLock poisoned, recovering");
        poisoned.into_inner()  // Data is still valid
    })
}
```

### Error Handling Pattern

```rust
// BAD
let name = gateway.metadata.name.unwrap();

// GOOD
let name = gateway.metadata.name.as_ref()
    .ok_or_else(|| anyhow!("Gateway missing name"))?;
```

---

## TDD WORKFLOW

**RED → GREEN → REFACTOR** - Always.

### RED: Write Failing Test First

```rust
#[tokio::test]
async fn test_router_maglev_distribution() {
    let router = Router::new();
    let backends = vec![
        Backend::new(ip1, 8080, 100),
        Backend::new(ip2, 8080, 100),
    ];

    router.add_route(HttpMethod::GET, "/api", backends).unwrap();

    // Verify even distribution across 10K requests
    // ...
}
```

### GREEN: Minimal Implementation

Write just enough code to make the test pass.

### REFACTOR: Clean Up

Extract helpers, improve naming, add edge cases. Tests must still pass.

---

## KEY PATTERNS IN THE CODEBASE

### Maglev Load Balancing

```rust
// Build lookup table (65,537 entries)
let table = maglev_build_compact_table(&backends);

// O(1) backend selection
let hash = fnv1a_hash(path, client_ip, client_port);
let backend_idx = table[hash % table.len()];
```

### Router with matchit + Maglev

```rust
pub struct Router {
    prefix_router: RwLock<matchit::Router<RouteKey>>,  // Path matching
    routes: RwLock<HashMap<RouteKey, Route>>,          // Route data
}

pub struct Route {
    pattern: Arc<str>,
    backends: Vec<Backend>,
    maglev_table: Vec<u8>,  // Compact Maglev lookup
}
```

### Connection Pooling

```rust
// Per-backend, per-protocol pools
pub struct BackendPool {
    http1_pools: HashMap<BackendKey, Http1Pool>,
    http2_pools: HashMap<BackendKey, Http2Pool>,
}

// Reuse connections
let conn = pool.get_or_create(backend).await?;
conn.send_request(req).await
```

### Circuit Breaker

```rust
pub struct CircuitBreaker {
    state: AtomicU8,  // Closed, Open, HalfOpen
    failure_count: AtomicU32,
    last_failure: AtomicU64,
}

// 50% error rate threshold → open circuit
// 30 seconds timeout → half-open
// 1 success in half-open → close
```

---

## VERIFICATION CHECKLIST

Before every commit:

```bash
# Format
cargo fmt

# Lint (treat warnings as errors)
cargo clippy -- -D warnings

# Tests
cargo test

# Full CI locally
make ci-local
```

---

## COMMON TASKS

### Adding a New Filter Type

1. Add variant to `FilterAction` enum in `filters.rs`
2. Implement filter logic in `apply_request_filters()` or `apply_response_filters()`
3. Parse from HTTPRoute in `http_route.rs`
4. Add tests

### Adding a New Matching Condition

1. Add to `RouteMatch` struct
2. Update `matches_request()` in `router.rs`
3. Parse from HTTPRoute in `http_route.rs`
4. Add tests

### Adding a New Metric

1. Register in `metrics.rs`
2. Instrument in relevant code path
3. Add to `/metrics` endpoint tests

---

## FILE LOCATIONS

| What | Where |
|------|-------|
| Main entry point | `control/src/main.rs` |
| Router + Maglev | `control/src/proxy/router.rs` |
| HTTP server | `control/src/proxy/server.rs` |
| TLS support | `control/src/proxy/tls.rs` |
| Connection pools | `control/src/proxy/backend_pool.rs` |
| Circuit breaker | `control/src/proxy/circuit_breaker.rs` |
| Rate limiter | `control/src/proxy/rate_limiter.rs` |
| GatewayClass controller | `control/src/apis/gateway/gateway_class.rs` |
| Gateway controller | `control/src/apis/gateway/gateway.rs` |
| HTTPRoute controller | `control/src/apis/gateway/http_route.rs` |
| EndpointSlice watcher | `control/src/apis/gateway/endpointslice_watcher.rs` |
| Shared types | `common/src/lib.rs` |
| K8s manifests | `deploy/` |

---

## DEPENDENCIES

| Crate | Purpose |
|-------|---------|
| `kube` 1.0 | Kubernetes API client + controller runtime |
| `gateway-api` 0.16 | Gateway API CRD types |
| `hyper` 1.0 | HTTP/1.1 and HTTP/2 |
| `hyper-util` 0.1 | Hyper utilities |
| `tokio` 1.41 | Async runtime |
| `tokio-rustls` 0.26 | TLS with rustls |
| `matchit` 0.8 | Radix tree for path matching |
| `regex` 1.11 | Header/query regex matching |
| `prometheus` 0.14 | Metrics |
| `tracing` 0.1 | Structured logging |
| `tikv-jemalloc` 0.6 | Memory allocator |

---

## AGENT INSTRUCTIONS

When working on this codebase:

1. **Read first** - Understand existing patterns before changing
2. **TDD always** - Write failing test, implement, refactor
3. **Use safe_read/safe_write** - Never raw `.unwrap()` on locks
4. **Small commits** - One logical change per commit
5. **Run checks** - `make ci-local` before pushing

**This is a learning project** - code quality matters, but we're exploring and experimenting. Ask questions if unclear.

---

## ROADMAP

**Stage 1 (Complete):**
- Gateway API controller
- HTTP proxy with all features
- TLS, HTTP/2, connection pooling
- Health checking, rate limiting

**Stage 2 (Planned):**
- WASM plugin system (wasmtime)
- Plugin SDK
- Built-in plugins
