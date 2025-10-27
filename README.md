# RAUTA

**Kernel-accelerated Kubernetes Ingress Controller**

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

RAUTA is an experimental ingress controller that uses eBPF to route HTTP traffic directly in the Linux kernel, bypassing traditional userspace proxies for simple requests.

> **Status**: 🚧 Experimental - Active Development

## What We're Building

Most HTTP requests follow simple patterns: `GET /api/users` or `POST /orders`. These don't need complex regex matching or protocol translation - they just need fast packet forwarding to the right backend pod.

RAUTA handles these common cases in the kernel using eBPF (extended Berkeley Packet Filter), achieving sub-microsecond latency. Complex requests that need full HTTP/2 support or regex routing fall back to a Rust-based userspace proxy.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         Client                               │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
         ┌───────────────────────────────────┐
         │   Tier 1: XDP (eXpress Data Path) │
         │   • HTTP/1.1 GET requests         │
         │   • Exact path match: /api/users  │
         │   • Latency: <1μs                 │
         │   • 60% of traffic                │
         └───────────┬───────────────────────┘
                     │ miss
                     ▼
         ┌───────────────────────────────────┐
         │   Tier 2: TC-BPF                  │
         │   • HTTP POST/PUT/DELETE          │
         │   • Prefix match: /api/*          │
         │   • Latency: ~10μs                │
         │   • 30% of traffic                │
         └───────────┬───────────────────────┘
                     │ miss
                     ▼
         ┌───────────────────────────────────┐
         │   Tier 3: Rust Userspace          │
         │   • HTTP/2, gRPC, TLS             │
         │   • Regex: /users/[0-9]+          │
         │   • Latency: ~100μs               │
         │   • 10% of traffic                │
         └───────────┬───────────────────────┘
                     │
                     ▼
         ┌───────────────────────────────────┐
         │       Backend Pods (K8s)          │
         └───────────────────────────────────┘
```

## How It Works

**Tier 1 (XDP)** - Fastest path, kernel-only:
- Parses HTTP/1.1 GET requests at the NIC driver level
- Looks up exact paths in an eBPF hash map (ROUTES)
- Selects backend using per-route compact Maglev table (MAGLEV_TABLES)
- O(1) consistent hashing with connection affinity (LRU flow cache)
- Encapsulates packet (IPIP) and forwards directly to pod
- Never touches userspace

**Tier 2 (TC-BPF)** - Fast path, kernel-only:
- Handles HTTP POST/PUT/DELETE methods
- Uses longest prefix matching for paths like `/api/*`
- Same Maglev hashing and IPIP encapsulation
- Still in kernel, just after network stack allocation

**Tier 3 (Rust)** - Complex path, userspace:
- Full HTTP/2 and gRPC support
- Regex path matching
- TLS termination
- Built with tokio + hyper + rustls

## Kubernetes Integration

RAUTA watches Kubernetes Ingress resources and automatically updates eBPF maps:

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
        pathType: Exact
        backend:
          service:
            name: user-service
            port:
              number: 8080
```

The controller compiles this into eBPF map entries that route traffic at line rate.

## Load Balancing

RAUTA uses **per-route compact Maglev consistent hashing** for backend selection:

- **Per-route tables**: Each route gets its own 4KB Maglev table (vs. single global 262KB table)
- **Compact design**: 4099-entry tables with u8 indices (supports up to 32 backends per route)
- **O(1) lookup**: Constant-time backend selection using pre-computed permutations
- **Minimal disruption**: ~1/N re-balancing when backends change (Google Maglev algorithm)
- **Connection affinity**: LRU flow cache for consistent per-connection routing
- **Stack-safe**: Separate map storage avoids BPF 512-byte stack limit

See [documents/maglev-architecture.md](documents/maglev-architecture.md) for detailed design and performance analysis.

## Performance Goals

- **Tier 1 latency**: <1μs p99 (XDP exact match)
- **Tier 2 latency**: ~10μs p99 (TC-BPF prefix match)
- **Tier 3 latency**: ~100μs p99 (Rust userspace)
- **Throughput**: 1M+ requests/sec per core (Tier 1)
- **Memory**: <100MB baseline, <5MB per 1000 routes

## Technology Stack

- **eBPF**: Aya framework (Rust eBPF toolkit)
- **Userspace**: Rust with tokio, hyper, rustls
- **Kubernetes**: kube-rs client library
- **Load balancing**: Maglev algorithm
- **Encapsulation**: IPIP (20-byte overhead)

## Current Status

**✅ Tier 1 (XDP) Implemented** - HTTP/1.1 parsing and XDP_TX forwarding complete!

### What's Working

- ✅ HTTP/1.1 method parsing (GET, POST, PUT, DELETE, HEAD, PATCH, OPTIONS)
- ✅ FNV-1a path hashing for fast route lookups
- ✅ **Per-route compact Maglev consistent hashing** (4KB per route, stack-safe)
- ✅ LRU flow affinity cache (Cilium pattern)
- ✅ XDP_TX hairpin NAT with checksum recalculation
- ✅ Per-CPU metrics (lock-free counters)
- ✅ 22 unit tests passing (TDD approach)

### Quick Start (Cross-Platform)

**One-command setup** - auto-detects your platform:

```bash
./scripts/setup.sh
```

**Linux** 🐧 (Full Native Toolchain):
```bash
# Everything works natively!
cd bpf && cargo +nightly build --release --target=bpfel-unknown-none
cd common && cargo test
```

**macOS** 🍎 (Hybrid Workflow):
```bash
# Fast unit tests
cd common && cargo test

# Build BPF in Docker (when needed)
./docker/build.sh
```

See [DEVELOPMENT.md](DEVELOPMENT.md) for platform-specific guides.

### Docker (All Platforms)

```bash
# Build RAUTA (compiles BPF + control plane)
./docker/build.sh

# Run integration tests
./docker/test.sh

# Benchmark performance
./docker/benchmark.sh
```

See [docker/README.md](docker/README.md) for details.

> 💡 **Build Issues?** Check [BUILD.md](BUILD.md) for platform-specific troubleshooting (macOS LLVM, Linux deps, CI setup)

### Project Structure

```
rauta/
├── common/          # Shared types (Pod-compatible for BPF)
├── bpf/             # XDP program (HTTP parsing + forwarding)
│   ├── src/main.rs        # XDP entry point (~330 lines)
│   └── src/forwarding.rs  # Packet forwarding (~240 lines)
├── control/         # Control plane (Aya framework)
│   └── src/main.rs        # BPF loader + metrics (~240 lines)
├── tests/           # Integration tests
└── docker/          # Docker build environment
```

### What's Next

- ⏳ Tier 2 (TC-BPF) - Prefix matching with BPF LPM tries
- ⏳ Tier 3 (Rust userspace) - HTTP/2, gRPC, regex matching
- ⏳ Kubernetes integration - Watch Ingress resources
- ⏳ CLI tool - `rautactl` for route management

RAUTA is a learning project exploring the boundaries of kernel networking and Kubernetes integration.

## Name

**Rauta** (Finnish: "iron") - Named after the element that Rust prevents, fitting the Finnish naming theme of our tooling ecosystem.

## License

Apache 2.0
