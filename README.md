# RAUTA

**Kubernetes Gateway API controller in Rust**

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

---

## What is this?

A learning project building a Kubernetes Gateway API controller from scratch in Rust.

Currently implements:
- GatewayClass, Gateway, HTTPRoute controllers
- Dynamic listener management (multiple Gateways can share ports)
- EndpointSlice-based backend discovery
- Maglev consistent hashing for load balancing

## Status

**In Development** - Core Gateway API reconciliation works. HTTP routing in progress.

What works:
- ✅ GatewayClass reconciliation
- ✅ Gateway reconciliation with shared listeners
- ✅ HTTPRoute parsing and validation
- ✅ Service → EndpointSlice resolution
- ✅ Maglev load balancer
- ⏳ HTTP request routing (in progress)

## Architecture

### How it works

```
┌─────────────────────────────────────────────────────────────┐
│                     Kubernetes Cluster                      │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ Gateway API Resources                                │  │
│  │                                                      │  │
│  │  GatewayClass ──> Gateway ──> HTTPRoute             │  │
│  │      │              │              │                 │  │
│  │      └──────────────┴──────────────┘                 │  │
│  │                     │                                │  │
│  └─────────────────────┼────────────────────────────────┘  │
│                        │ watch events                      │
│                        ▼                                    │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ RAUTA Pod (DaemonSet - runs on each node)           │  │
│  │                                                      │  │
│  │  ┌────────────────────────────────────────────────┐ │  │
│  │  │ Controllers (kube-rs)                          │ │  │
│  │  │  • GatewayClass reconciler                     │ │  │
│  │  │  • Gateway reconciler → ListenerManager        │ │  │
│  │  │  • HTTPRoute reconciler → Router               │ │  │
│  │  │  • EndpointSlice watcher → Backend discovery   │ │  │
│  │  └───────────────────┬────────────────────────────┘ │  │
│  │                      │ updates                       │  │
│  │                      ▼                                │  │
│  │  ┌────────────────────────────────────────────────┐ │  │
│  │  │ Shared Listeners (ports 80/443)                │ │  │
│  │  │  • Multiple Gateways share same ports          │ │  │
│  │  │  • Dynamic listener creation                    │ │  │
│  │  └───────────────────┬────────────────────────────┘ │  │
│  │                      │ routes to                     │  │
│  │                      ▼                                │  │
│  │  ┌────────────────────────────────────────────────┐ │  │
│  │  │ Router (matchit + Maglev)                      │ │  │
│  │  │  • Path matching (prefix, exact)               │ │  │
│  │  │  • Maglev load balancing                       │ │  │
│  │  │  • Backend health tracking                     │ │  │
│  │  └───────────────────┬────────────────────────────┘ │  │
│  │                      │ proxies to                    │  │
│  │                      ▼                                │  │
│  │  ┌────────────────────────────────────────────────┐ │  │
│  │  │ Backend Pods                                   │ │  │
│  │  │  Pod 1 (10.0.1.5:8080) ◄──┐                   │ │  │
│  │  │  Pod 2 (10.0.1.6:8080) ◄──┼── EndpointSlices  │ │  │
│  │  │  Pod 3 (10.0.1.7:8080) ◄──┘                   │ │  │
│  │  └────────────────────────────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### Deployment model

RAUTA runs as a **DaemonSet** - one pod per node:
- Binds to host network ports (80/443)
- Watches Gateway API resources cluster-wide
- Dynamically creates listeners based on Gateway specs
- Routes traffic to backend pods via EndpointSlices

## Quick Start

**Requirements:**
- Rust 1.75+
- Kubernetes cluster with Gateway API CRDs

```bash
# Clone
git clone https://github.com/yairfalse/rauta
cd rauta

# Build
cargo build --release

# Deploy to Kubernetes
kubectl apply -f deploy/rauta-daemonset.yaml

# Create Gateway
kubectl apply -f examples/gateway.yaml
```

## Development

```bash
# Run tests
cargo test

# Format
cargo fmt

# Lint
cargo clippy
```

## Why?

Learning project to explore:
- Rust async (tokio, hyper)
- Kubernetes controllers (kube-rs)
- Gateway API internals
- TDD in Rust

## Tech Stack

- **tokio** - async runtime
- **hyper** - HTTP/2
- **kube-rs** - Kubernetes client
- **matchit** - path matching
- **prometheus** - metrics

## Name

**Rauta** = Finnish for "iron"

## License

Apache 2.0

---

**Built for learning. Shared for others learning too.** 🦀
