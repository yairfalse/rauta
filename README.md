# RAUTA ⚙️

**A Kubernetes Gateway API implementation in Rust** - Fast, safe, and extensible.

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

---

## What is RAUTA?

A Kubernetes ingress controller built in Rust with HTTP/2 support and WASM-based extensibility.

**Core Features:**
- Modern Kubernetes Gateway API (v1) support
- HTTP/2 connection pooling with multiplexing
- Maglev consistent hashing for load balancing
- WASM plugin system for safe extensibility
- Production-grade observability (Prometheus metrics)

**Why RAUTA?**
- **Memory Safe** - Rust prevents entire classes of bugs
- **Modern Standards** - Gateway API native, not legacy Ingress
- **Extensible** - WASM plugins for safe extensibility
- **Fast** - HTTP/2 multiplexing, efficient connection pooling

---

## Status

**Stage 1: Gateway API Controller** ✅ Complete
- GatewayClass, Gateway, HTTPRoute reconciliation
- Dynamic backend resolution via EndpointSlice API
- Maglev load balancing with consistent hashing
- Prefix matching (e.g., `/api` matches `/api/users/123`)
- Prometheus metrics

**Stage 2: HTTP/2 Connection Pooling** ✅ Complete
- HTTP/2 multiplexing (1 connection → 44K+ requests)
- Per-backend connection pools with circuit breakers
- 3-state health tracking (Healthy → Degraded → Unhealthy)
- Protocol detection (auto-fallback to HTTP/1.1)
- Validated: 14K+ req/sec in load testing

**Stage 3: WASM Plugin System** 🚧 In Design
- Safe extensibility without memory leaks
- Custom authentication, rate limiting, transforms
- Sandboxed execution environment

---

## Architecture

```
┌─────────────────────────────────────────────┐
│   Kubernetes API Server                     │
│   - GatewayClass                            │
│   - Gateway                                 │
│   - HTTPRoute                               │
└──────────────────┬──────────────────────────┘
                   │ Watch events
                   ▼
┌─────────────────────────────────────────────┐
│   RAUTA Controller (Rust)                   │
│   - kube-rs watchers                        │
│   - Reconcile loop                          │
│   - EndpointSlice → Pod IP resolution       │
│   - Prometheus metrics                      │
└──────────────────┬──────────────────────────┘
                   │ Update routes
                   ▼
┌─────────────────────────────────────────────┐
│   Router (in-memory)                        │
│   - matchit radix tree (prefix match)       │
│   - Maglev hash table (per route)           │
│   - Backend selection: O(1)                 │
└──────────────────┬──────────────────────────┘
                   │ HTTP/2 pooling
                   ▼
┌─────────────────────────────────────────────┐
│   HTTP/2 Connection Pools                   │
│   - Per-backend pools                       │
│   - Multiplexed streams                     │
│   - Circuit breaker (3-state)               │
│   - Auto protocol detection                 │
└─────────────────────────────────────────────┘
```

---

## Quick Start

```bash
# Clone and build
git clone https://github.com/yairfalse/rauta
cd rauta
cargo build --release

# Run controller (requires KUBECONFIG)
./target/release/control

# Deploy in Kubernetes
kubectl apply -f manifests/
```

**Requirements:**
- Rust 1.75+
- Kubernetes cluster (kind/minikube/production)
- KUBECONFIG configured

---

## HTTP/2 Connection Pooling

RAUTA implements production-grade HTTP/2 connection pooling:

**Features:**
- **Multiplexing** - Single connection handles thousands of requests
- **Circuit Breaker** - Automatic degradation on backend failures
  - 5 failures → Degraded (50% capacity reduction)
  - 10 failures → Unhealthy (circuit open for 30s)
- **Protocol Detection** - Auto-detects HTTP/2 support, falls back to HTTP/1.1
- **Connection Reuse** - Minimize TCP handshakes and TLS negotiation

**Prometheus Metrics:**
```
http2_pool_connections_active{backend}
http2_pool_connections_created_total{backend}
http2_pool_connections_failed_total{backend}
http2_pool_requests_queued_total{backend}
```

**Load Test Results:**
- 14,362 req/sec sustained throughput
- 703μs average latency
- 44,543:1 multiplexing ratio (1 connection served 44K requests)
- Zero failures, zero queuing

See [`docs/HTTP2_CONNECTION_POOLING.md`](docs/HTTP2_CONNECTION_POOLING.md) for design details.

---

## Design Choices

**Why Gateway API instead of Ingress?**

Gateway API is the modern Kubernetes standard (v1 since Oct 2023). More expressive and role-oriented than Ingress.

**Why Maglev for load balancing?**

Consistent hashing keeps connections sticky to the same backend. When backends change, only ~1/N connections get redistributed. Maglev is Google's algorithm - fast (O(1) lookup) and proven at scale.

**Why HTTP/2?**

- Request multiplexing (1 connection → thousands of requests)
- Header compression (HPACK)
- Research-backed: ACM 2024 shows HTTP/2 outperforms HTTP/3 by 45%

**Why WASM for plugins?**

- Memory safe and sandboxed execution
- Can't crash the proxy process
- Platform-independent bytecode
- Compile once, run anywhere

**Why Rust?**

- Memory safety (no segfaults in production)
- Strong type system (catch bugs at compile time)
- Excellent async ecosystem (tokio, hyper)

---

## Technology Stack

**Userspace:**
- **tokio** - Async runtime
- **hyper** - HTTP/1.1 and HTTP/2
- **kube-rs** - Kubernetes API client
- **gateway-api** - Official Gateway API CRD types
- **matchit** - Radix tree for path matching
- **prometheus** - Metrics

**Future:**
- **wasmtime** - WASM runtime for plugins
- **io_uring** - Zero-copy I/O (Linux 5.7+)

---

## Development

### Build and Test

```bash
# Build
cargo build

# Run tests
cargo test

# Format
cargo fmt

# Lint
cargo clippy -- -D warnings
```

### Pre-commit Hook

Quality checks run automatically on commit:

```bash
✅ cargo fmt --check
✅ cargo clippy
✅ cargo test
```

Commit fails if code isn't formatted, has clippy warnings, or tests fail.

### TDD Workflow

All features follow Test-Driven Development:

1. **RED**: Write failing test
2. **GREEN**: Minimal implementation to pass
3. **REFACTOR**: Improve code quality
4. **COMMIT**: Small, focused commits

See `CLAUDE.md` for detailed guidelines.

---

## Roadmap

**Completed:**
- ✅ Gateway API controller (GatewayClass, Gateway, HTTPRoute)
- ✅ EndpointSlice resolution for dynamic backends
- ✅ Maglev load balancing
- ✅ HTTP/2 connection pooling with circuit breakers
- ✅ Prometheus metrics

**In Progress:**
- 🚧 WASM plugin system design
- 🚧 TLS termination
- 🚧 Per-core worker architecture (lock-free routing)

**Future Exploration:**
- io_uring zero-copy I/O
- eBPF sockmap for service mesh mode
- Multi-cluster routing

---

## Contributing

**How to help:**
1. **Try it** - Deploy in a cluster, report issues
2. **Review code** - Suggest improvements
3. **Improve docs** - Make things clearer
4. **Share ideas** - What features would be useful?

**Before contributing:**
- Read `CLAUDE.md` (project guidelines)
- Follow TDD workflow (tests before code)
- Keep commits small and focused
- No TODOs in code (finish features or document why)

---

## Naming

**Rauta** (Finnish: "iron") - Part of the Finnish tool naming theme:
- **TAPIO**: Kubernetes observer 🌲
- **AHTI**: Event correlation 🌊
- **RAUTA**: Ingress controller ⚙️
- **URPO**: Trace explorer 🔍

Built in Rust, the language that prevents memory bugs.

---

## License

Apache 2.0 - Free and open source.

---

## Links

- **GitHub**: https://github.com/yairfalse/rauta
- **Issues**: https://github.com/yairfalse/rauta/issues
- **Docs**: See `docs/` directory

---

**Fast. Safe. Extensible.** 🦀
