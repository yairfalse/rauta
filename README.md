# RAUTA ⚙️

**A Kubernetes Gateway API implementation in Rust** - Experimental, learning in public.

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

---


This is a **week-old project**. We're learning Rust, eBPF, and Kubernetes networking as we go.

---

## What is RAUTA?

A Kubernetes ingress controller built in Rust. We're exploring:
- Modern Kubernetes Gateway API (v1)
- Consistent hashing for load balancing (Maglev)
- eBPF for observability (future work)

**Why another ingress controller?**
- **Learning project** - We wanted to understand how ingress controllers work
- **Modern APIs** - Gateway API is newer than Ingress, wanted to try it
- **Rust** - Memory-safe systems programming
- **eBPF exploration** - Eventually use eBPF for deep HTTP observability

---

## What's Working Now?

✅ **Stage 1: Gateway API Controller** (Complete as of Week 1)

| Feature | Status | Notes |
|---------|--------|-------|
| GatewayClass reconciliation | ✅ | Watches GatewayClass resources |
| Gateway reconciliation | ✅ | Updates status, manages listeners |
| HTTPRoute reconciliation | ✅ | Path matching, backend resolution |
| Service → Pod IP resolution | ✅ | Via Kubernetes Endpoints API |
| Maglev load balancing | ✅ | Consistent hashing for backends |
| Prefix matching | ✅ | `/api/users` matches `/api/users/123` |
| Tests | ✅ | 92 tests passing |
| Metrics | ✅ | Prometheus /metrics endpoint |


---

## How It Works (Current)

```
1. Watch Kubernetes resources (Gateway API v1)
   ├─ GatewayClass: rauta.io/gateway-controller
   ├─ Gateway: Listeners (HTTP on port 80)
   └─ HTTPRoute: Routes with backend refs

2. Resolve Services → Pod IPs
   └─ Kubernetes Endpoints API

3. Route requests using Maglev consistent hashing
   ├─ Hash(path, src_ip, src_port) → backend
   └─ Sticky connections (same flow → same backend)

4. Update Kubernetes status
   └─ Accepted, ResolvedRefs conditions
```

That's it. Simple L7 HTTP routing in Rust userspace.

---

## Quick Start

```bash
# Clone and build
git clone https://github.com/yairfalse/rauta
cd rauta
cargo build --release

# Run tests
cargo test

# Run controller (requires KUBECONFIG)
./target/release/control

# Deploy in Kubernetes
kubectl apply -f manifests/
```

**Requirements:**
- Rust 1.75+
- Kubernetes cluster (kind/minikube OK)
- KUBECONFIG configured

---

## Architecture (Current)

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
│   - Router (matchit + Maglev)               │
│   - Prometheus metrics                      │
└──────────────────┬──────────────────────────┘
                   │ Update routes
                   ▼
┌─────────────────────────────────────────────┐
│   Router (in-memory)                        │
│   - matchit radix tree (prefix match)       │
│   - Maglev hash table (per route)           │
│   - Backend selection: O(1)                 │
└─────────────────────────────────────────────┘
```

**No eBPF yet.** Pure Rust userspace. Simple.

---

## 🔍 eBPF Observability (The Vision)

**Status: Designed, not implemented.** This is our research direction for Stage 2.

### The Approach

Most ingress controllers give you request counts and latency histograms. RAUTA will add **kernel-level HTTP visibility** via eBPF XDP observation.

**Key Principle:** eBPF captures, userspace analyzes. XDP programs always return `XDP_PASS` (observation only, never routing).

### Technical Architecture

```
┌─────────────────────────────────────────────┐
│  XDP Program (eBPF)                         │
│  - Parse: Ethernet → IP → TCP → HTTP       │
│  - Extract: method, path, status, timing   │
│  - Emit: Ring buffer event                  │
│  - Return: XDP_PASS (always!)               │
└──────────────────┬──────────────────────────┘
                   │ bpf_ringbuf_output()
                   ▼
┌─────────────────────────────────────────────┐
│  Rust Processors (Userspace)               │
│  - Baseline learning (p50/p99 per route)   │
│  - Anomaly detection (statistical)          │
│  - Event emission (TAPIO format)            │
└─────────────────────────────────────────────┘
```

**eBPF Program** (simplified):
```c
SEC("xdp")
int rauta_observe(struct xdp_md *ctx) {
    struct http_event evt = {
        .timestamp_ns = bpf_ktime_get_ns(),
        .method = parse_method(data, data_end),
        .path_hash = hash_path(data, data_end),
        .status = parse_status(data, data_end),
    };

    bpf_ringbuf_output(&EVENTS, &evt, sizeof(evt), 0);
    return XDP_PASS;  // Never drop packets
}
```

**Userspace Processor** (Rust):
```rust
struct SlowResponseProcessor {
    baselines: HashMap<u64, LatencyBaseline>,  // path_hash → baseline
}

impl Processor for SlowResponseProcessor {
    fn process(&mut self, evt: &HttpEvent) -> Option<ObserverEvent> {
        let baseline = self.baselines.get(&evt.path_hash)?;
        let latency_ms = evt.response_time_ns / 1_000_000;

        // Detect: p99 > 2x baseline
        if latency_ms > baseline.p99 * 2.0 {
            Some(ObserverEvent {
                type_: "slow_response",
                route: evt.path,
                latency_ms,
                baseline_p99: baseline.p99,
                backend_ip: evt.backend_ip,
            })
        } else {
            None
        }
    }
}
```

### What We'll Detect

**1. Slow Response Detection**
- Learn per-route latency baselines (p50, p95, p99)
- Emit event when p99 > 2x baseline
- Correlate to backend pod via IP

**2. TCP Connection Failures**
- Hook: `kprobe/tcp_set_state`
- Detect: `SYN_SENT → CLOSE` (connection refused)
- Emit before HTTP layer even tries

**3. HTTP Error Spikes**
- Track per-route error rate baseline
- Detect: error_rate > baseline + 3σ (statistical)
- Identify failing backend pod

**4. Latency Breakdown**
- Timestamp: XDP receive, socket send, socket recv, HTTP complete
- Breakdown: network vs application time
- Identify bottleneck layer

**Research Citations:**
- ACM 2024: "QUIC is not Quick Enough" (HTTP/2 > HTTP/3)
- Kernel Recipes 2024: io_uring zero-copy (+31-43% throughput)
- Cilium May 2024: eBPF sockmap (30% latency reduction)

Full design: [`docs/OBSERVABILITY_ARCHITECTURE.md`](docs/OBSERVABILITY_ARCHITECTURE.md)


---

## Technology Stack

**Userspace:**
- **tokio** - Async runtime
- **kube-rs** - Kubernetes API client
- **gateway-api** - Official Gateway API CRD types
- **matchit** - Radix tree for path matching (used by axum)
- **prometheus** - Metrics

**Future (eBPF):**
- **aya** - Rust eBPF framework
- **XDP** - Packet observation (not routing!)

---

## Development

### Prerequisites

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install pre-commit dependencies
cargo install cargo-fmt cargo-clippy
```

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

The project enforces quality checks on every commit:

```bash
# Automatically runs on: git commit
✅ cargo fmt --check
✅ cargo clippy
✅ cargo test
```

**Commit fails if:**
- Code isn't formatted
- Clippy warnings exist
- Tests fail

This keeps the codebase clean. See `.git/hooks/pre-commit`.

### TDD Workflow

**All features follow Test-Driven Development:**

1. **RED**: Write failing test
2. **GREEN**: Minimal implementation to pass
3. **REFACTOR**: Improve code quality
4. **COMMIT**: Small, focused commits

See `CLAUDE.md` for detailed TDD guidelines.

---

## Design Choices

**Why Gateway API instead of Ingress?**

Gateway API is the newer Kubernetes standard (v1 as of Oct 2023). It's more expressive and role-oriented than Ingress. We wanted to learn it.

**Why Maglev for load balancing?**

Consistent hashing keeps connections sticky to the same backend. When backends change, only ~1/N connections get redistributed. Maglev is Google's algorithm for this - it's fast (O(1) lookup) and well-tested at scale.

**Why matchit for routing?**

Kubernetes needs prefix matching (`/api/users` → `/api/users/123`). Can't use a HashMap for that. matchit is a radix tree used by axum - it's fast and battle-tested.

**Why Rust?**

- Memory safety (no segfaults in production)
- Strong type system (catch bugs at compile time)
- Good ecosystem (tokio, kube-rs, aya)
- Learning opportunity

---

## Contributing

**This is a learning project.** We're figuring things out as we go.

**How to help:**
1. **Try it** - Run in a kind cluster, report bugs
2. **Review code** - Suggest improvements
3. **Improve docs** - Help explain better
4. **Share ideas** - What features would be useful?

**Before contributing:**
- Read `CLAUDE.md` (project guidelines)
- Follow TDD workflow (tests before code)
- Keep commits small (<30 lines preferred)
- No TODOs in code (finish features or document why)

---

## Naming

**Rauta** (Finnish: "iron") - Part of the Finnish tool naming theme:
- **TAPIO**: Kubernetes observer 🌲
- **AHTI**: Event correlation 🌊
- **RAUTA**: Ingress controller ⚙️

Built in Rust, the language that prevents memory bugs (like rust on iron).

---

## License

Apache 2.0 - Free and open source.

---

## Links

- **GitHub**: https://github.com/yairfalse/rauta
- **Issues**: https://github.com/yairfalse/rauta/issues
- **Docs**: See `docs/` and `documents/` directories

---