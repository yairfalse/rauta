# RAUTA ⚙️

**Iron-clad routing at wire speed** - Experimental Rust + eBPF Ingress Controller

[![CI](https://github.com/yairfalse/rauta/actions/workflows/ci.yml/badge.svg)](https://github.com/yairfalse/rauta/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

> **Status**: 🚧 Stage 1 Core Complete - Pure Rust HTTP proxy with advanced routing. Additional Stage 1 features (K8s Integration, Observability) in progress.

---

## What is RAUTA?

A Kubernetes Ingress Controller that's **fast** and **simple**.

**The Idea:**
Most of your traffic hits the same 100 routes. Those should be **really fast**. The long tail of routes? Those can be a bit slower but need to handle complex logic.

So we built two layers:
1. **Hot cache in eBPF** - Handles your top routes at kernel speed
2. **Full router in Rust** - Handles everything else with rich features

Think of it like CPU caching: L1 cache (eBPF) for hot data, RAM (Rust) for everything else.

---

## How It Works

```
Client Request: GET /api/users/123
        │
        ▼
┌─────────────────────────────────────────────┐
│  eBPF Layer (Stage 2 - Coming Soon)        │
│                                             │
│  Hash("/api/users")                         │
│  → Check hot routes map                     │
│  → Found! Route to 10.0.1.5:8080           │
│  → Done in <10 microseconds                 │
│                                             │
│  If NOT in cache ↓                          │
└─────────────────────────────────────────────┘
        │
        ▼
┌─────────────────────────────────────────────┐
│  Rust Layer (Stage 1 - ✅ Working Now)      │
│                                             │
│  1. Match route: /api/users → user-service  │
│     (supports prefix matching, wildcards)   │
│                                             │
│  2. Pick backend with Maglev hashing:       │
│     Flow(path, src_ip, port) → backend #2   │
│                                             │
│  3. Forward request to 10.0.1.2:8080       │
│     Done in <100 microseconds               │
│                                             │
│  4. Update hot routes if this gets popular  │
└─────────────────────────────────────────────┘
```

**Why this design?**
- 99% of your traffic hits the same routes → eBPF makes them blazing fast
- 1% of traffic needs complex routing → Rust handles that
- Best of both worlds: speed + features

---

## Current Status: Stage 1 ✅

**What's Working NOW:**

| Feature | Status | Notes |
|---------|--------|-------|
| HTTP/1.1 Router | ✅ | matchit radix tree |
| Prefix Matching | ✅ | K8s PathType: Prefix |
| Exact Matching | ✅ | K8s PathType: Exact |
| Load Balancing | ✅ | Maglev consistent hashing |
| Flow Distribution | ✅ | path + src_ip + src_port |
| Tests | ✅ | 68 tests passing |
| Pre-commit Hook | ✅ | fmt + clippy + tests |

**Try it NOW:**

```bash
# Clone and build
git clone https://github.com/yairfalse/rauta
cd rauta
cargo build --release

# Run HTTP proxy (Stage 1)
./target/release/control

# Test routing
curl http://127.0.0.1:8080/api/users
# → Route matched! Backend: 10.0.1.1:8080

curl http://127.0.0.1:8080/api/users/123
# → Route matched! Backend: 10.0.1.2:8080 (prefix match!)
```

---

## Performance Goals

**Right Now (Stage 1):**
- Routing: <100 microseconds per request
- Throughput: 100K+ requests/second
- Algorithm: O(log n) route lookup + O(1) backend selection

**Coming Soon (Stage 2 with eBPF):**
- Hot routes: <10 microseconds (in kernel, no context switch)
- Cache hit rate: 99% of traffic
- Everything else falls back to Stage 1

**How?**
- Your top 100 routes get cached in kernel memory
- eBPF decides which backend without leaving kernel space
- No malloc, no TCP stack overhead for hot paths
- It's basically free routing for popular endpoints

---

## Roadmap

### ✅ Stage 1: Pure Rust Proxy (Week 1-8) - COMPLETE

- [x] HTTP server (hyper 1.0)
- [x] matchit router (prefix + exact matching)
- [x] Maglev load balancing
- [x] Flow-based hashing
- [x] TDD workflow (RED → GREEN → REFACTOR)
- [x] Pre-commit hook (fmt + clippy + tests)

### 🔄 Stage 1 Continued: K8s Integration (Week 3-4)

- [ ] Ingress watcher (kube-rs)
- [ ] EndpointSlice watcher
- [ ] Service discovery
- [ ] Dynamic route updates

### 🔄 Stage 1 Continued: Observability (Week 5-8)

- [ ] Per-route metrics (requests, latency, errors)
- [ ] Prometheus /metrics endpoint
- [ ] Health checks
- [ ] Simple web UI

### ⏳ Stage 2: eBPF Hot Cache (Week 9-16)

- [ ] XDP program (Aya-rs)
- [ ] BPF maps (routes, backends, flow cache)
- [ ] HTTP parsing in XDP (method + path extraction)
- [ ] Top-100 route identification
- [ ] Userspace sync (hot → eBPF cache)
- [ ] Metrics via ring buffer

### ⏳ Stage 3: Production Features (Week 17+)

- [ ] TLS termination (userspace)
- [ ] HTTP/2 support
- [ ] WebSocket support
- [ ] Advanced routing (weighted, canary, A/B)
- [ ] OTEL traces from XDP

---

## Technology Stack

**Userspace (Rust):**
- **tokio** - Async runtime
- **hyper 1.0** - HTTP server and client
- **matchit** - Radix tree for prefix matching (used by axum)
- **kube-rs** - Kubernetes API client
- **aya** - eBPF framework (Stage 2)

**Kernel (eBPF):**
- **XDP** - eXpress Data Path (pre-TCP packet processing)
- **Aya BPF** - Rust eBPF programs (Stage 2)
- **BPF maps** - Shared state (routes, backends, flow cache)

**Why Rust + eBPF?**
- **Memory safety** - No segfaults, no UAF bugs
- **Zero-copy XDP** - Packets processed before TCP stack
- **Strong typing** - BPF maps are type-safe (Aya)
- **TDD-friendly** - Easy to test userspace logic

---

## Development

### Setup

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone project
git clone https://github.com/yairfalse/rauta
cd rauta

# Build
cargo build
cargo test
```

### Pre-commit Hook (Automatic Quality Checks)

The project uses a pre-commit hook that runs on every commit:

```bash
# Already installed when you clone! Located at .git/hooks/pre-commit
# Runs automatically on: git commit

# What it checks:
✅ cargo fmt (format)
✅ cargo clippy (lint)
✅ cargo test (68 tests)
```

**Commit fails if:**
- Code is not formatted
- Clippy warnings exist
- Tests fail

No more nonsense commits! 🎉

### TDD Workflow (Strict RED → GREEN → REFACTOR)

**All code follows Test-Driven Development:**

```bash
# 1. RED: Write failing test
cargo test test_router_prefix_matching
# ❌ test_router_prefix_matching ... FAILED

# 2. GREEN: Minimal implementation to pass
# (write just enough code)
cargo test
# ✅ test_router_prefix_matching ... ok

# 3. REFACTOR: Improve code quality
# (add edge cases, improve design)
cargo test
# ✅ All tests still pass

# 4. COMMIT: Small, focused commits
git add . && git commit -m "feat: Add prefix matching (TDD)"
# Pre-commit hook runs automatically
```

**See `CLAUDE.md` for full TDD guidelines.**

---

## Design Choices

**Why matchit for routing?**
Kubernetes Ingress needs prefix matching (`/api/users` should match `/api/users/123`). A HashMap won't work for that - you need a tree structure. matchit is a radix tree that does exactly this, and it's fast (200ns lookups).

**Why Maglev for load balancing?**
Consistent hashing keeps connections sticky to the same backend. When backends change, only ~1/N connections need to move. Maglev is just a clever way to do this in O(1) time with a lookup table. Google's been using it for years.

**Why split eBPF + Rust?**
Because your traffic isn't evenly distributed. 99% of requests hit the same 100 routes. Those can live in kernel memory and route instantly. The other 1% gets the full Rust router with all the features. You get speed where it matters and flexibility where you need it.

---

## false-systems Ecosystem

**Finnish Tool Naming Theme:**

| Tool | Finnish | Meaning | Purpose |
|------|---------|---------|---------|
| **TAPIO** | 🌲 Tapio | Forest spirit | K8s observer (eBPF + Go) |
| **RAUTA** | ⚙️ Rauta | Iron | Ingress controller (Rust + eBPF) |
| **AHTI** | 🌊 Ahti | Water spirit | Correlation engine (Go) |
| **URPO** | 🔍 Urpo | Explorer | Trace explorer (Rust) |
| **ELAVA** | 💚 Elävä | Living | AWS scanner (Go) |

**Integration:**
- RAUTA → NATS → TAPIO (correlate ingress with pod metrics)
- RAUTA → OTLP → URPO (trace visualization)
- RAUTA → NATS → AHTI (service graph)

All projects are **experimental** and **learning in public**.

---

## Contributing

**This is a learning project!** We're figuring things out as we go.

**How to help:**
1. **Try RAUTA** - Run Stage 1 locally, give feedback
2. **Review code** - Especially eBPF experts (for Stage 2)
3. **Improve docs** - Help us explain better
4. **Report bugs** - Issues welcome!
5. **Suggest features** - What would make this useful?

**Before contributing:**
- Read `CLAUDE.md` (project guidelines)
- Follow TDD workflow (RED → GREEN → REFACTOR)
- Pre-commit hook will enforce code quality

**Code style:**
- NO TODOs in code (complete features or document why)
- NO stubs (finish what you start)
- TDD mandatory (tests before code)
- Small commits (<30 lines preferred)

---

## Benchmarks (Coming Soon)

We'll test with real traffic patterns to see:
- How fast is matchit routing in practice?
- What's the actual throughput we can handle?
- How much faster is eBPF for hot routes?
- Where are the bottlenecks?

Numbers coming once we have K8s integration working!

---

## Why "Iron"?

**Rauta** (Finnish: "iron") - The element that Rust prevents.

**Naming philosophy:**
- RAUTA prevents memory bugs (like rust on iron)
- Built with Rust (the language)
- Iron-clad routing (reliable, strong)
- Finnish naming theme (like TAPIO, AHTI)

---

## License

Apache 2.0 - Free and open source.

Built with ❤️ and 🦀 by the false-systems team.

---

## Links

- **GitHub**: https://github.com/yairfalse/rauta
- **Issues**: https://github.com/yairfalse/rauta/issues
- **CI**: https://github.com/yairfalse/rauta/actions
- **Docs**: See `docs/` directory

---

**RAUTA: Iron-clad routing at wire speed** ⚙️🦀

*Experimental. Learning in public. Join us!*
