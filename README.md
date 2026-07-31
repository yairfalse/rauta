<p align="center">
  <br>
  <code>R A U T A</code>
  <br>
  <br>
  AI-native Kubernetes API gateway. Lock-free. Agent-operable.
  <br>
  <br>
  <a href="https://github.com/false-systems/rauta/actions/workflows/ci.yml"><img src="https://github.com/false-systems/rauta/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/tests-220%2B-brightgreen" alt="Tests">
  <img src="https://img.shields.io/badge/rust-1.83%2B-f74c00" alt="Rust">
  <img src="https://img.shields.io/badge/license-Apache%202.0-blue" alt="License">
</p>

---

RAUTA is a Rust Kubernetes Gateway API controller and L7 HTTP proxy. It routes traffic with Maglev consistent hashing, manages backends with lock-free circuit breakers and rate limiters, and exposes everything to AI agents via MCP.

```
cargo build --release -p control      # gateway
cargo build --release -p rauta-cli    # CLI + kubectl plugin
cargo test --workspace                # 220+ tests, 5 crates
```

The current implementation focuses on the gateway data plane, Gateway API reconciliation, diagnostics, and read-oriented agent access. The strategic roadmap is documented in [docs/plan-agentic-gateway.md](docs/plan-agentic-gateway.md): ontology, temporal state, safe agent actions, and optional eBPF evidence.

---

## How it works

RAUTA watches Kubernetes Gateway API resources (GatewayClass, Gateway, HTTPRoute, EndpointSlice, Secret) and translates them into a routing table. Every HTTP request hitting the proxy gets:

1. **Routed** — radix tree path match, then Maglev consistent hash to pick a backend
2. **Filtered** — request/response header modification, redirects, timeouts, retries
3. **Protected** — lock-free circuit breaker + rate limiter before forwarding
4. **Forwarded** — HTTP/1.1 or HTTP/2 via per-worker connection pools

```
Client ──► Listener ──► Router ──► Filters ──► Forwarder ──► Backend
                                      │
              :9091 Admin API ◄───────┘
              MCP Tools (11)
              Diagnostics Engine (7 rules)
              Prometheus /metrics
```

The admin server on `:9091` is completely separate from the data plane — you can always query gateway state even under load.

---

## Lock-free hot path

Every request goes through the hot path. No locks touch health data or rate/circuit state:

| What | Old | New |
|------|-----|-----|
| Circuit breaker state check | 5 RwLock reads | 1 AtomicU64 load |
| Rate limiter token acquire | 3 RwLock writes | 1 AtomicU64 CAS |
| Backend health + drain check | 2 RwLock reads | 1 ArcSwap load (~1ns) |
| Tried-backends tracking | HashSet (heap alloc) | u32 bitmask (stack) |
| Hop-by-hop header check | to_lowercase() (heap) | eq_ignore_ascii_case (zero-alloc) |
| Filter cloning for RouteMatch | Deep clone | Arc::clone (~1ns) |

The circuit breaker packs `state`, `failure_count`, `success_count`, and `half_open_requests` into a single `u64` and uses CAS loops for all transitions. The rate limiter packs tokens (16.16 fixed-point) and a timestamp into another `u64`.

---

## Agent API

Three ways to talk to a running gateway. Same data model, different interfaces:

### MCP — for AI agents

13 tools are defined for Claude Code, Cursor, or any MCP-compatible client:

```
rauta_status                 rauta_list_routes           rauta_get_route
rauta_list_circuit_breakers  rauta_list_rate_limiters    rauta_diagnose
rauta_drain_backend          rauta_undrain_backend       rauta_cache_stats
rauta_list_listeners         rauta_metrics_snapshot      rauta_timeline
rauta_diff
```

Read paths such as status, routes, circuit breakers, rate limiters, listeners, cache stats, metrics, timeline, semantic diffs, and diagnostics are wired through the admin API. Drain and undrain are exposed as operator surfaces but currently return explicit unavailable errors until the safe-actions spec is implemented.

### CLI — for humans and scripts

```bash
rauta status                                  # table (default)
rauta routes list --format=json               # machine
rauta metrics snapshot --format=json          # structured metrics
rauta backends health                         # backend state from route snapshots
rauta diagnose circuit-breaker-cascade --format=agent  # LLM-optimized
rauta diagnose degraded --since-seconds=300 --format=agent
rauta timeline --since-seconds=300            # recent events and compact snapshots
rauta diff --since-seconds=300                # semantic recent-state diff
rauta backends drain 10.0.1.5:8080            # explicit unavailable until safe actions
```

The `--format=agent` output includes `_meta` and `_hints` blocks designed for LLM consumption. The binary is also available as `kubectl-rauta`.

### REST — for everything else

```
GET  /api/v1/status             GET  /api/v1/routes
GET  /api/v1/cache              GET  /api/v1/metrics
GET  /api/v1/circuit-breakers   GET  /api/v1/rate-limiters
GET  /api/v1/listeners          GET  /api/v1/timeline?since_seconds=300
GET  /api/v1/diff?since_seconds=300
POST /api/v1/diagnose?symptom=...&since_seconds=300
POST /api/v1/backends/drain     POST /api/v1/backends/undrain  # 501 unavailable
GET  /healthz
```

---

## Diagnostics engine

Deterministic rules. No LLM. Each produces a structured diagnosis with a causal chain, evidence, and actionable commands:

| Rule | What it detects | Severity |
|------|-----------------|----------|
| `RAUTA-CB-001` | 2+ circuit breakers open simultaneously (cascade) | Critical |
| `RAUTA-CB-002` | Single circuit breaker open | Warning |
| `RAUTA-RL-001` | Rate limit bucket exhausted (0 tokens) | Warning |
| `RAUTA-BE-001` | Route with zero healthy backends | Critical |
| `RAUTA-BE-002` | All backends draining on a route | Warning |
| `RAUTA-CACHE-001` | Route cache hit rate below 50% | Info |
| `RAUTA-LISTEN-001` | Multiple listeners competing for same port | Info |

```bash
$ rauta diagnose circuit-breaker-cascade
[Critical] RAUTA-CB-001 (circuit-breaker-cascade)
  Cause: 2 backends have open circuit breakers
  Cause: Multiple simultaneous failures suggest upstream dependency issue
  Evidence: Backend 10.0.1.1:8080 is Open (failures: 5)
  Evidence: Backend 10.0.1.2:8080 is Open (failures: 5)
  Action: Check if backends share a common upstream dependency (rauta backends health)
```

Agent-facing diagnostic responses also include `ontology_evidence`: schema-versioned entities and evidence attributes for routes, backends, listeners, circuit breakers, rate limiters, and cache state. Existing human-readable `evidence` strings remain for compatibility.

---

## Gateway API

Kubernetes Gateway API v1 support for the core HTTP gateway path:

- **Resources**: GatewayClass, Gateway, HTTPRoute, EndpointSlice, Secret
- **Matching**: path prefix (radix tree), headers (exact + regex), query params, methods
- **Filters**: request/response header modification, redirects (301/302), timeouts, retries with exponential backoff
- **Load balancing**: Maglev consistent hashing — O(1) lookup, weighted, sticky via `(client_ip, port)` hash, ~1/N disruption on backend changes

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: api
spec:
  parentRefs:
  - name: my-gateway
  rules:
  - matches:
    - path:
        type: PathPrefix
        value: /api/v1
    backendRefs:
    - name: api-service
      port: 8080
      weight: 90
    - name: api-canary
      port: 8080
      weight: 10
```

---

## Project layout

```
rauta/
  common/         no_std shared types — HttpMethod, Backend, Maglev, RouteKey
  control/        gateway controller + proxy + admin server (main binary)
  agent-api/      GatewayQuery trait, snapshot types, diagnostics engine
  mcp-server/     MCP tool definitions for AI agent integration
  rauta-cli/      CLI + kubectl-rauta plugin
  deploy/         K8s manifests, Grafana dashboards, Prometheus config
  docs/           architecture docs, research, ADRs
  scripts/        dev scripts, load tests, git hooks
  manifests/      raw K8s YAML (namespace, RBAC, DaemonSet)
  docker/         Dockerfiles for various build targets
```

---

## Quick start

```bash
# standalone (no K8s)
RAUTA_BACKEND_ADDR=127.0.0.1:9090 ./target/release/control

# kubernetes
RAUTA_K8S_MODE=true ./target/release/control

# admin API always available
curl localhost:9091/api/v1/status
```

| Variable | Default | Purpose |
|----------|---------|---------|
| `RAUTA_K8S_MODE` | `false` | Start K8s controllers |
| `RAUTA_BIND_ADDR` | `0.0.0.0:8080` | Proxy listen |
| `RAUTA_BACKEND_ADDR` | — | Standalone backend |
| `RAUTA_ADMIN_PORT` | `9091` | Admin server |
| `RAUTA_TLS_CERT` / `_KEY` | — | TLS termination |
| `RAUTA_GATEWAY_CLASS` | `rauta` | GatewayClass to watch |
| `RUST_LOG` | `info` | Log level |

---

## Development

```bash
cargo test --workspace                                    # all tests
cargo clippy --all-targets --all-features -- -D warnings  # lint
cargo fmt --all -- --check                                # format check
make ci-local                                             # full CI
```

Pre-commit and pre-push hooks enforce fmt, clippy, and tests.

## Roadmap

RAUTA's next major direction is agentic operation:

- **Ontology** — versioned gateway entities and diagnostic evidence are now present; actions and causal links are the next expansion.
- **Timeline** — bounded in-memory event journal, compact snapshot history, semantic diffs, and time-window diagnostics are present.
- **Safe actions** — bounded drain, undrain, quarantine, and verification loops.
- **eBPF evidence** — optional Linux kernel TCP signals feeding diagnostics.

See [docs/README.md](docs/README.md) and [docs/plan-agentic-gateway.md](docs/plan-agentic-gateway.md).

---

## Built with

[hyper](https://hyper.rs) ·
[tokio](https://tokio.rs) ·
[kube-rs](https://kube.rs) ·
[rustls](https://github.com/rustls/rustls) ·
[matchit](https://github.com/ibraheemdev/matchit) ·
[arc-swap](https://github.com/vorner/arc-swap) ·
[jemalloc](https://jemalloc.net) ·
[prometheus](https://github.com/tikv/rust-prometheus)

---

## Part of FALSE Systems

| Tool | Finnish | Role |
|------|---------|------|
| **RAUTA** | iron | API gateway |
| **AHTI** | god of the sea | Causality engine |
| **POLKU** | path | Event transport |
| **KULTA** | gold | Progressive delivery |
| **TAPIO** | forest god | Infrastructure management |

---

Apache 2.0
