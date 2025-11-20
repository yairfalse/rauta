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

```
Kubernetes API
    ↓ (watch)
Controllers (kube-rs)
    ↓ (update)
Router (matchit + Maglev)
    ↓ (proxy)
Backends (HTTP/2 pools)
```

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
