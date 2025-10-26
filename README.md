# RAUTA

**Kernel-accelerated Kubernetes Ingress Controller**

RAUTA is an experimental ingress controller that uses eBPF to route HTTP traffic directly in the Linux kernel, bypassing traditional userspace proxies for simple requests.

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
- Looks up exact paths in an eBPF hash map
- Selects backend using Maglev consistent hashing
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

RAUTA uses **Maglev consistent hashing** for backend selection:
- O(1) lookup time
- Minimal disruption when backends change (1/N)
- Per-connection consistent routing
- Backend health-aware failover

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

**Research and design phase.** We're validating the architecture and studying production eBPF load balancers to understand what works at scale.

RAUTA is a learning project exploring the boundaries of kernel networking and Kubernetes integration.

## Name

**Rauta** (Finnish: "iron") - Named after the element that Rust prevents, fitting the Finnish naming theme of our tooling ecosystem.

## License

Apache 2.0
