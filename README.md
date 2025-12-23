# RAUTA

**Kubernetes Gateway API Controller - Learning Project**

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.83%2B-orange.svg)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-201%2B-green.svg)]()

A Kubernetes Gateway API controller written in Rust. Built to learn Kubernetes networking, Rust async, and L7 proxy patterns.

**This is a learning project** - I'm building it to understand:
- How Kubernetes Gateway API works (kube-rs)
- L7 HTTP proxy patterns (hyper, tokio)
- Load balancing algorithms (Maglev consistent hashing)
- TLS termination and HTTP/2
- Connection pooling and health checking

**Current Status**: Stage 1 complete - full Gateway API controller with HTTP proxy.

---

## Features

| Feature | Description |
|---------|-------------|
| **Gateway API v1** | Full support for GatewayClass, Gateway, HTTPRoute |
| **HTTP/1.1 & HTTP/2** | Both cleartext (h2c) and over TLS |
| **TLS Termination** | SNI support, ALPN negotiation |
| **Maglev Load Balancing** | Google's consistent hashing algorithm |
| **Path Matching** | Prefix matching with radix tree (matchit) |
| **Header/Query Matching** | Exact and regex matching |
| **Request Filters** | Header modification, redirects |
| **Response Filters** | Header modification |
| **Rate Limiting** | Token bucket algorithm per route |
| **Circuit Breaker** | Passive health checking (error rate threshold) |
| **Connection Pooling** | Per-backend, per-protocol pools |
| **Retries** | Exponential backoff |
| **Prometheus Metrics** | Request counts, latencies per route |
| **Leader Election** | HA-ready (Kubernetes Lease) |

---

## Quick Start

```bash
# Clone and build
git clone https://github.com/yairfalse/rauta
cd rauta
cargo build --release

# Run tests (201+ test cases)
cargo test

# Run in standalone mode (no Kubernetes)
RAUTA_BACKEND_ADDR=127.0.0.1:9090 \
RAUTA_BIND_ADDR=127.0.0.1:8080 \
./target/release/control

# Run in Kubernetes mode
RAUTA_K8S_MODE=true ./target/release/control
```

**Requirements:**
- Rust 1.83+
- Kubernetes 1.28+ (for K8s mode)
- Gateway API CRDs installed

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         RAUTA Controller                                 │
│                                                                          │
│  ┌────────────────────────────────────────────────────────────────────┐ │
│  │  Kubernetes Controllers (kube-rs)                                  │ │
│  │  ├── GatewayClass reconciler                                       │ │
│  │  ├── Gateway reconciler → ListenerManager                          │ │
│  │  ├── HTTPRoute reconciler → Router                                 │ │
│  │  ├── EndpointSlice watcher → Dynamic backend discovery             │ │
│  │  └── Secret watcher → TLS certificates                             │ │
│  └────────────────────────────────────────────────────────────────────┘ │
│                                    │                                     │
│                                    ▼                                     │
│  ┌────────────────────────────────────────────────────────────────────┐ │
│  │  HTTP Proxy (hyper + tokio)                                        │ │
│  │  ├── Router: matchit radix tree + Maglev hash tables               │ │
│  │  ├── Connection pools: HTTP/1.1 and HTTP/2 per backend             │ │
│  │  ├── TLS: rustls with SNI + ALPN                                   │ │
│  │  ├── Filters: headers, redirects, timeouts                         │ │
│  │  └── Health: circuit breaker, rate limiter                         │ │
│  └────────────────────────────────────────────────────────────────────┘ │
│                                    │                                     │
│                                    ▼                                     │
│  ┌────────────────────────────────────────────────────────────────────┐ │
│  │  Observability                                                     │ │
│  │  └── Prometheus /metrics endpoint                                  │ │
│  └────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────┘
```

### Request Flow

```
Client Request
      │
      ▼
┌─────────────┐
│  Listener   │ (TCP accept, TLS handshake if HTTPS)
└──────┬──────┘
       │
       ▼
┌─────────────┐
│   Router    │ (path match → Maglev → backend selection)
└──────┬──────┘
       │
       ▼
┌─────────────┐
│  Filters    │ (request headers, auth checks)
└──────┬──────┘
       │
       ▼
┌─────────────┐
│  Forwarder  │ (connection pool → backend request)
└──────┬──────┘
       │
       ▼
┌─────────────┐
│  Filters    │ (response headers)
└──────┬──────┘
       │
       ▼
Client Response
```

### Maglev Load Balancing

RAUTA uses Google's Maglev algorithm for consistent hashing:

- **O(1) lookup** - Hash to backend in constant time
- **Minimal disruption** - ~1/N connections move when backends change
- **Weighted** - Supports weighted backend distribution
- **Sticky** - Same (client_ip, port) → same backend

```
Hash(path, client_ip, client_port)
           │
           ▼
   ┌───────────────┐
   │ Maglev Table  │  (65,537 entries)
   │ [0] → B1      │
   │ [1] → B2      │
   │ [2] → B1      │
   │ ...           │
   │ [N] → B3      │
   └───────────────┘
           │
           ▼
     Selected Backend
```

---

## Gateway API Example

```yaml
# GatewayClass
apiVersion: gateway.networking.k8s.io/v1
kind: GatewayClass
metadata:
  name: rauta
spec:
  controllerName: rauta.io/gateway-controller

---
# Gateway
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: my-gateway
spec:
  gatewayClassName: rauta
  listeners:
  - name: http
    port: 80
    protocol: HTTP
  - name: https
    port: 443
    protocol: HTTPS
    tls:
      mode: Terminate
      certificateRefs:
      - name: my-cert

---
# HTTPRoute
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: api-route
spec:
  parentRefs:
  - name: my-gateway
  hostnames:
  - "api.example.com"
  rules:
  - matches:
    - path:
        type: PathPrefix
        value: /api/v1
      method: GET
    - headers:
      - name: X-API-Version
        value: "2"
    backendRefs:
    - name: api-service
      port: 8080
      weight: 90
    - name: api-canary
      port: 8080
      weight: 10
    filters:
    - type: RequestHeaderModifier
      requestHeaderModifier:
        add:
        - name: X-Request-ID
          value: "{{uuid}}"
    - type: ResponseHeaderModifier
      responseHeaderModifier:
        add:
        - name: X-Served-By
          value: "rauta"
```

---

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RAUTA_K8S_MODE` | `false` | Enable Kubernetes mode |
| `RAUTA_BIND_ADDR` | `0.0.0.0:8080` | Listen address (standalone) |
| `RAUTA_BACKEND_ADDR` | - | Backend address (standalone) |
| `RAUTA_TLS_CERT` | - | TLS certificate path |
| `RAUTA_TLS_KEY` | - | TLS key path |
| `RAUTA_GATEWAY_CLASS` | `rauta` | GatewayClass name to watch |
| `RAUTA_LOG_LEVEL` | `info` | Log level |
| `RUST_LOG` | `info` | Tracing log level |

### Endpoints

| Port | Endpoint | Purpose |
|------|----------|---------|
| 8080 | `/` | HTTP proxy (configurable) |
| 9090 | `/metrics` | Prometheus metrics |
| 9090 | `/healthz` | Liveness probe |
| 9090 | `/readyz` | Readiness probe |

---

## Project Structure

```
rauta/
├── common/                          # Shared types (no_std compatible)
│   └── src/lib.rs                   # HttpMethod, Backend, Maglev, RouteKey
├── control/                         # Main controller
│   └── src/
│       ├── main.rs                  # Entry point
│       ├── apis/gateway/            # Kubernetes controllers
│       │   ├── gateway_class.rs     # GatewayClass reconciler
│       │   ├── gateway.rs           # Gateway reconciler
│       │   ├── http_route.rs        # HTTPRoute reconciler
│       │   ├── endpointslice_watcher.rs  # Pod discovery
│       │   └── secret_watcher.rs    # TLS secrets
│       └── proxy/                   # HTTP proxy
│           ├── router.rs            # Route matching + Maglev
│           ├── server.rs            # HTTP server
│           ├── listener_manager.rs  # Dynamic listeners
│           ├── request_handler.rs   # Request routing
│           ├── forwarder.rs         # Backend forwarding
│           ├── filters.rs           # Request/response filters
│           ├── tls.rs               # TLS termination
│           ├── backend_pool.rs      # Connection pooling
│           ├── http1_pool.rs        # HTTP/1.1 pool
│           ├── circuit_breaker.rs   # Passive health
│           ├── rate_limiter.rs      # Token bucket
│           ├── health_checker.rs    # Active health (TCP probes)
│           └── metrics.rs           # Prometheus
└── deploy/                          # Kubernetes manifests
    ├── rauta-daemonset.yaml
    └── gateway-api.yaml
```

---

## Development

### Build & Test

```bash
# Build
cargo build --release

# Run tests (201+ test cases)
cargo test

# Lint
cargo clippy -- -D warnings

# Format
cargo fmt

# Run all CI checks locally
make ci-local
```

### Local Development

```bash
# Create kind cluster
kind create cluster --name rauta-dev

# Install Gateway API CRDs
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml

# Run controller
RAUTA_K8S_MODE=true cargo run
```

---

## Tech Stack

| Component | Purpose |
|-----------|---------|
| [kube](https://kube.rs) | Kubernetes API client + controller runtime |
| [hyper](https://hyper.rs) | HTTP/1.1 and HTTP/2 server/client |
| [tokio](https://tokio.rs) | Async runtime |
| [tokio-rustls](https://github.com/rustls/tokio-rustls) | TLS termination |
| [gateway-api](https://gateway-api.sigs.k8s.io) | Gateway API CRD types |
| [matchit](https://github.com/ibraheemdev/matchit) | Radix tree for path matching |
| [prometheus](https://github.com/tikv/rust-prometheus) | Metrics |
| [tracing](https://tracing.rs) | Structured logging |
| [jemalloc](https://jemalloc.net/) | Memory allocator (better for async) |

---

## Design Decisions

**Why Gateway API instead of Ingress?**

Gateway API is the modern Kubernetes standard (v1 as of Oct 2023). It's more expressive and role-oriented than Ingress.

**Why Maglev for load balancing?**

Consistent hashing keeps connections sticky to the same backend. When backends change, only ~1/N connections get redistributed. Maglev is Google's algorithm - O(1) lookup and proven at scale.

**Why userspace L7 (not eBPF)?**

HTTP/2 and TLS require TCP reassembly which XDP can't do. Even Cilium uses Envoy for L7. eBPF is great for L3/L4, but L7 belongs in userspace.

**Why Rust?**

Memory safety without garbage collection. Good ecosystem (tokio, kube-rs, hyper). Learning opportunity.

---

## Roadmap

**Stage 1 (Complete):**
- Gateway API controller
- HTTP proxy with routing
- TLS termination
- Load balancing
- Health checking
- Metrics

**Stage 2 (Planned):**
- WASM plugin system (wasmtime)
- Plugin SDK (Rust, Go, TypeScript)
- Built-in plugins (JWT, CORS, rate-limit)

---

## Naming

**Rauta** (Finnish: "iron") - Part of a Finnish tool naming theme:
- **RAUTA** (iron) - Gateway API controller
- **KULTA** (gold) - Progressive delivery controller

---

## License

Apache 2.0

---

**Learning Rust. Learning K8s. Building tools.**
