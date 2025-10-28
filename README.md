# RAUTA

**Simple Kubernetes Ingress Controller**

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

> **Status**: 🚧 Experimental - We're trying our best to make this work

---

## What is RAUTA?

RAUTA is an experimental ingress controller for Kubernetes. Our goal is to make ingress as easy as possible.

**What we're building:**
- Standard K8s Ingress resources (no custom config)
- Simple deployment (one kubectl command)
- Built-in observability (see what's happening)
- Written in Rust (memory safe, fast)

**This is a learning project.** We're building in public and figuring things out as we go.

---

## Quick Start

```bash
# Install RAUTA
kubectl apply -f https://rauta.io/install.yaml

# Create standard Ingress
kubectl apply -f - <<EOF
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: my-app
spec:
  rules:
  - host: myapp.com
    http:
      paths:
      - path: /
        pathType: Prefix
        backend:
          service:
            name: my-service
            port:
              number: 8080
EOF
```

---

## Architecture

```
┌─────────────────────────────────────────┐
│         Kubernetes Cluster              │
│                                         │
│  ┌─────────┐  ┌─────────┐             │
│  │ Service │  │ Service │             │
│  │  (pods) │  │  (pods) │             │
│  └────▲────┘  └────▲────┘             │
│       │            │                    │
│       └────────────┘                    │
│                │                        │
│         ┌──────▼──────┐                │
│         │    RAUTA    │                │
│         │  (2 pods)   │                │
│         └──────▲──────┘                │
│                │                        │
└────────────────┼────────────────────────┘
                 │
            Client requests
```

**How it works:**
1. Watches K8s Ingress resources
2. Watches Service/Endpoints for backend IPs
3. Routes HTTP traffic to backend pods
4. Logs requests for debugging
5. Exports metrics to Prometheus

Pure Rust userspace proxy. Simple.

---

## Features

### Current Status (MVP in progress)

- ✅ HTTP/1.1 routing
- ✅ Maglev load balancing
- ✅ Request logging
- ✅ Prometheus metrics
- 🔄 K8s Ingress sync (in progress)
- 🔄 Backend health checks (in progress)

### Planned

- ⏳ TLS termination
- ⏳ HTTP/2 support
- ⏳ WebSocket support
- ⏳ Simple web UI

**Note:** This is experimental. Features may change.

---

## Observability

RAUTA tries to make it easy to see what's happening:

**Request Logs:**
```bash
curl http://rauta:9000/requests

# Shows recent requests:
GET /api/users 200 12ms → 10.0.1.42:8080
POST /orders 201 45ms → 10.0.2.15:8080
```

**Metrics:**
```bash
curl http://rauta:9000/metrics

# Prometheus metrics:
# rauta_requests_total
# rauta_request_duration_seconds
# rauta_backend_errors_total
```

---

## Configuration

RAUTA uses standard Kubernetes Ingress resources:

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: example
spec:
  rules:
  - host: api.example.com
    http:
      paths:
      - path: /users
        pathType: Prefix
        backend:
          service:
            name: user-service
            port:
              number: 8080
```



---

## Development

### Build

```bash
git clone https://github.com/yairfalse/rauta
cd rauta

cargo build --release
cargo test
```

### Run Locally

```bash
export KUBECONFIG=~/.kube/config
cargo run --bin rauta
```

---

## false-systems Ecosystem

RAUTA can integrate with other false-systems tools:

**The Stack:**
- **TAPIO**: K8s observer (eBPF + Go)
- **RAUTA**: Ingress controller (Rust)
- **AHTI**: Correlation engine (Go)
- **URPO**: Trace explorer (Rust)

**Integration:**
- RAUTA → NATS → TAPIO (correlate ingress with pod metrics)
- RAUTA → OTLP → URPO (trace visualization)
- RAUTA → NATS → AHTI (service graph)

RAUTA works standalone, but can plug into the ecosystem if you want.

All projects are experimental and named after Finnish mythology/elements.

---

## Contributing

This is a community learning project. We're figuring things out as we go.

**Ways to help:**
- Try RAUTA and share feedback
- Report bugs or issues
- Suggest improvements
- Contribute code (any size welcome!)

See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

---

## Project Goals

**What we're trying to do:**
- Make K8s ingress as simple as possible
- Use standard K8s resources (Ingress)
- Make it easy to see what's happening
- Learn Rust and K8s together
- Build something useful for small teams

---
## Why Rust?

We chose Rust because:
- Memory safe (fewer bugs)
- Fast (good performance)
- Good ecosystem (tokio, hyper, kube-rs)
- Fun to learn

---

## Name

**Rauta** (Finnish: "iron")

Named after the element that Rust prevents. Part of our Finnish naming theme.

---

## License

Apache 2.0 - Free and open source.

---
