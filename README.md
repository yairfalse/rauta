# RAUTA ⚙️

**A Kubernetes Gateway API implementation in Rust** - Fast, safe, and extensible.

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![Gateway API](https://img.shields.io/badge/Gateway%20API-v1.2.0-purple.svg)](https://gateway-api.sigs.k8s.io/)
[![Tests](https://img.shields.io/badge/tests-90%20passing-brightgreen.svg)]()
[![Performance](https://img.shields.io/badge/throughput-129K%20req%2Fs-orange.svg)]()
<sub>Note: Badge shows average throughput on 12-core machine; ADR claims are peak per-core/node throughput under ideal conditions.</sub>

**Tech Stack:**
[![Tokio](https://img.shields.io/badge/async-tokio-blue.svg?logo=rust)](https://tokio.rs)
[![Hyper](https://img.shields.io/badge/HTTP-hyper-blue.svg?logo=rust)](https://hyper.rs)
[![Kubernetes](https://img.shields.io/badge/K8s-kube--rs-326CE5.svg?logo=kubernetes&logoColor=white)](https://kube.rs)
[![HTTP/2](https://img.shields.io/badge/HTTP%2F2-enabled-success.svg)]()
[![Prometheus](https://img.shields.io/badge/metrics-prometheus-E6522C.svg?logo=prometheus&logoColor=white)](https://prometheus.io)

---

## What is RAUTA?

A learning project exploring Rust and Kubernetes Gateway API - built for fun, happens to be fast.

**What's Actually Built:**
- Kubernetes Gateway API (v1) controller (GatewayClass, Gateway, HTTPRoute)
- HTTP/2 connection pooling with multiplexing
- Multi-core workers (129K+ req/sec on 12 cores)
- Maglev consistent hashing for load balancing
- Weighted routing for canary deployments (90/10 splits)
- Passive health checking with circuit breakers
- Graceful shutdown with connection draining
- Connection/request timeouts (5s/30s)
- Prometheus metrics and structured logging
- 74 unit tests + integration tests

**Why Build This?**
- Learn Rust async (tokio, hyper)
- Understand Kubernetes controllers (kube-rs)
- Explore HTTP/2 multiplexing and connection pooling
- Practice TDD (every feature test-first)
- Have fun building systems software

---

## What Works

**Gateway API Controller** ✅
- GatewayClass, Gateway, HTTPRoute reconciliation
- Dynamic backend resolution via EndpointSlice API
- Service port → targetPort resolution
- Prefix matching (e.g., `/api` matches `/api/users/123`)

**Load Balancing** ✅
- Maglev consistent hashing (Google's algorithm)
- Weighted routing for canary deployments (90/10 splits)
- Backend health tracking (passive health checks)
- Connection draining for graceful removal

**HTTP/2 Performance** ✅
- Multi-core workers (12 workers → 129K req/sec)
- HTTP/2 multiplexing (1 connection → thousands of requests)
- Per-worker connection pools (lock-free)
- Auto-fallback to HTTP/1.1

**Reliability** ✅
- Connection timeout (5s for dead backends)
- Request timeout (30s for slow backends)
- Circuit breakers (3-state: Healthy → Degraded → Unhealthy)
- Graceful shutdown with connection draining (SIGTERM-safe)

**Observability** ✅
- Prometheus metrics (request rates, latencies, pool stats)
- Structured logging (OpenTelemetry-style fields)
- Per-request tracing with request IDs

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
- **Peak: 129,813 req/sec** (12 workers, 400 concurrent connections)
- **Average Latency: 3.00ms** (p99 under 17ms)
- **Total: 3.9M requests** in 30 seconds
- Zero failures, zero dropped connections

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
- Reduces connection overhead

**Why multi-core workers?**

- Lock-free routing (each worker has own connection pools)
- Linear scaling with CPU cores (4 cores → 4x throughput)
- Zero contention under load

**Why Rust?**

- Memory safety (no segfaults)
- Strong type system (catch bugs at compile time)
- Excellent async ecosystem (tokio, hyper)
- Zero-cost abstractions

---

## Technology Stack

- **tokio** - Async runtime
- **hyper** - HTTP/1.1 and HTTP/2
- **kube-rs** - Kubernetes API client
- **gateway-api** - Official Gateway API CRD types
- **matchit** - Radix tree for path matching
- **prometheus** - Metrics
- **jemalloc** - Allocator optimized for async workloads

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


## Using This Code

This is a learning project, but the code is:
- Well-tested (74 unit tests + integration tests)
- Documented (see `docs/` directory)
- Following Rust best practices

**If you want to learn from it:**
- Read `CLAUDE.md` for TDD workflow
- Check `docs/` for design decisions
- Tests show how features work

**If you want to use it:**
- It works! But it's a learning project
- No guarantees, no roadmap
- Use at your own risk

---

## Naming

**Rauta** (Finnish: "iron") 

---

## License

Apache 2.0 - Free and open source.

---

## Links

- **GitHub**: https://github.com/yairfalse/rauta
- **Issues**: https://github.com/yairfalse/rauta/issues
- **Docs**: See `docs/` directory

---

**Built for fun. Happens to be fast.** 🦀
