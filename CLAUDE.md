# RAUTA: K8s-Native L7 Ingress Controller - Rust + eBPF Observability

**RAUTA = Iron-clad routing with deep visibility** ⚡🦀

## ⚠️ CRITICAL: Project Nature

**THIS IS A PRODUCTION-FOCUSED LEARNING PROJECT**
- **Goal**: Build a production-ready K8s ingress controller in Rust with eBPF observability
- **Language**: 100% Rust (userspace L7 proxy) + eBPF (observability)
- **Status**: 🚧 ACTIVE DEVELOPMENT - Building in public
- **Performance Target**: Competitive with NGINX/Envoy, with superior observability

## 🎯 PROJECT MISSION

**Mission**: Build a K8s-native ingress controller that combines production-ready L7 routing with deep eBPF-powered observability.

**Inspiration**:
- **NGINX Ingress**: Battle-tested L7 proxy, but limited observability
- **Cilium**: eBPF for L3/L4, Envoy for L7 (eBPF redirects to userspace)
- **Envoy**: Rich L7 features, but heavy and complex
- **RAUTA**: Rust userspace L7 proxy + eBPF observability (fast, observable, K8s-native)

**The Approach**: Follow proven architecture patterns (eBPF redirects to userspace L7 proxy), but build it in Rust with K8s-native integration and comprehensive observability.

## 🏗️ ARCHITECTURE PHILOSOPHY

### Reality-Based Design: eBPF Redirects, Userspace Routes

```
┌─────────────────────────────────────────────┐
│   eBPF Programs (kprobe/tracepoint)        │
│   - Connection tracking (TCP state)        │
│   - Latency measurement (per route)        │
│   - Request counting (per pod)             │
│   - NO L7 parsing (that's userspace!)      │
└──────────────────┬──────────────────────────┘
                   │ Ring Buffer (metrics)
                   ▼
┌─────────────────────────────────────────────┐
│   Rust L7 Proxy (control + data plane)    │
│   - HTTP/1.1, HTTP/2, gRPC routing         │
│   - TLS termination                         │
│   - Maglev load balancing                   │
│   - K8s Ingress/Gateway API sync            │
│   - Metrics export (Prometheus)             │
└─────────────────────────────────────────────┘
```

**Why This Works** (Learned from Cilium):
- **L7 routing in userspace** - Full HTTP/2, gRPC, TLS support
- **eBPF for observability** - Zero-overhead connection tracking, latency metrics
- **No XDP HTTP parsing** - That's not realistic for modern protocols
- **K8s-native** - Watch Ingress, EndpointSlices, Gateway API directly

**Traffic Flow**:
```
Client → TLS → Rust Proxy → HTTP/2/gRPC → Backend Pod
                    ↓
            eBPF observability
       (connection state, latency, errors)
```

## 🔥 PERFORMANCE REQUIREMENTS

### Target: Production-Ready L7 Performance

**Realistic Userspace L7 Targets**:
- **Request latency**: p99 < 5ms (competitive with NGINX/Envoy)
- **Throughput**: 100K+ requests/second per core
- **Memory**: <100MB resident for 10K routes
- **Connection handling**: 10K+ concurrent connections per instance

**Observability Overhead**:
- **eBPF metrics**: <1% CPU overhead
- **Prometheus scrape**: <100ms per scrape
- **Trace sampling**: Configurable (1%, 10%, 100%)

**NOT Targeting**:
- ❌ XDP-level latency (<10μs) - That's unrealistic for L7
- ❌ 10M pps - That's L4 load balancing (Katran), not L7 routing
- ✅ Competitive with NGINX/Envoy - That's achievable!

### Performance Patterns

```rust
// ✅ Async I/O with tokio
let proxy_task = tokio::spawn(async move {
    proxy_request(req, backend).await
});

// ✅ Connection pooling to backends
let client = hyper::Client::builder()
    .pool_max_idle_per_host(100)
    .build_http();

// ✅ Maglev consistent hashing (fast, stable)
fn select_backend(route_key: &RouteKey, backends: &[Backend]) -> Backend {
    let hash = maglev_hash(route_key);
    maglev_table[hash % TABLE_SIZE]
}

// ✅ Zero-copy eBPF metrics (per-CPU maps)
#[map]
static METRICS: PerCpuArray<RouteMetrics> =
    PerCpuArray::with_max_entries(1024, 0);
```

## 🦀 RUST + AYA REQUIREMENTS

### Language Requirements
- **THIS IS A RUST PROJECT** - All userspace code in Rust
- **eBPF Code**: C (stable) or Aya-RS (experimental Rust eBPF)
- **NO GO CODE** - Unlike Cilium's Go control plane, we use Rust everywhere
- **STRONG TYPING ONLY** - No `Box<dyn Any>` or runtime type checking

### Aya Framework Patterns (For Observability)

```rust
// ✅ Load eBPF program for observability
use aya::{Bpf, programs::KProbe};

let mut bpf = Bpf::load(include_bytes_aligned!(
    "../../target/bpfel-unknown-none/release/rauta-observer"
))?;

// Attach to TCP state changes
let program: &mut KProbe = bpf.program_mut("tcp_state_change")?.try_into()?;
program.load()?;
program.attach("tcp_set_state", 0)?;

// ✅ Access metrics from BPF maps
use aya::maps::PerCpuArray;

let mut metrics: PerCpuArray<_, RouteMetrics> =
    PerCpuArray::try_from(bpf.map_mut("ROUTE_METRICS")?)?;

// Aggregate per-CPU metrics
let per_cpu_values = metrics.get(&route_id, 0)?;
let total_requests: u64 = per_cpu_values.iter().map(|m| m.requests).sum();
```

**NO STUBS. NO TODOs. COMPLETE CODE ONLY.**

### TDD Workflow (RED → GREEN → REFACTOR)

**MANDATORY**: All code must follow strict Test-Driven Development

#### RED Phase: Write Failing Tests First
```rust
// Step 1: Write test that FAILS (RED)
#[tokio::test]
async fn test_http2_routing() {
    let router = Router::new();

    let backends = vec![Backend::new(
        u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100
    )];
    router.add_route(HttpMethod::GET, "/api/users", backends).unwrap();

    // Make HTTP/2 request
    let client = hyper::Client::builder()
        .http2_only(true)
        .build_http();

    let response = client.get("http://proxy/api/users".parse().unwrap())
        .await.expect("Request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
}

// Step 2: Verify test FAILS
// $ cargo test
// # test_http2_routing ... FAILED ✅ RED phase confirmed
```

#### GREEN Phase: Minimal Implementation
```rust
// Step 3: Write MINIMAL code to pass test
// server.rs - Add HTTP/2 support to hyper server
let server = hyper::server::conn::http2::Builder::new()
    .serve_connection(io, service)
    .await?;

// Step 4: Verify tests PASS
// $ cargo test
// # test_http2_routing ... ok ✅ GREEN phase confirmed
```

#### REFACTOR Phase: Improve Code Quality
```rust
// Step 5: Add gRPC test (builds on HTTP/2)
#[tokio::test]
async fn test_grpc_routing() {
    // Test gRPC request routing
}

// Step 6: Refactor for protocol negotiation (HTTP/1.1 vs HTTP/2)
async fn handle_connection(stream: TcpStream, router: Arc<Router>) {
    // Auto-detect ALPN or upgrade headers
    match negotiate_protocol(&stream).await {
        Protocol::Http1 => serve_http1(stream, router).await,
        Protocol::Http2 => serve_http2(stream, router).await,
    }
}

// Step 7: Verify tests still PASS after refactor
// $ cargo test
// # All tests ... ok ✅ REFACTOR complete
```

#### TDD Checklist
- [ ] **RED**: Write failing test first
- [ ] **RED**: Verify compilation fails or test fails
- [ ] **GREEN**: Write minimal implementation
- [ ] **GREEN**: Verify all tests pass
- [ ] **REFACTOR**: Add edge cases, improve design
- [ ] **REFACTOR**: Verify tests still pass
- [ ] **Commit**: `git add . && git commit -m "feat: ..."` (incremental)

## 🌐 PROTOCOL SUPPORT

### HTTP/1.1 (Stage 1 - Current)

```rust
use hyper::server::conn::http1;

let service = service_fn(|req| handle_request(req, router.clone()));
http1::Builder::new()
    .serve_connection(io, service)
    .await?;
```

✅ **Status**: Implemented
- Exact and prefix path matching
- Maglev load balancing
- Connection pooling

### HTTP/2 (Stage 2 - Week 2-3)

```rust
use hyper::server::conn::http2;

http2::Builder::new(TokioExecutor::new())
    .serve_connection(io, service)
    .await?;
```

**Requirements**:
- ALPN negotiation during TLS handshake
- Support for server push (optional)
- Handle GOAWAY frames gracefully

### gRPC (Stage 2 - Week 3-4)

```rust
// gRPC is HTTP/2 with specific content-type
if req.headers().get("content-type")
    .map(|v| v.as_bytes().starts_with(b"application/grpc"))
    .unwrap_or(false)
{
    // Route gRPC request
    // Add annotation support: backend-protocol: "GRPC"
}
```

**K8s Integration**:
```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  annotations:
    rauta.io/backend-protocol: "GRPC"
```

### TLS Termination (Stage 2 - Week 2)

```rust
use rustls::{ServerConfig, Certificate, PrivateKey};

let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));
let tls_stream = tls_acceptor.accept(tcp_stream).await?;

// ALPN negotiation for HTTP/2
let negotiated_protocol = tls_stream.negotiated_protocol();
match negotiated_protocol.as_deref() {
    Some(b"h2") => serve_http2(tls_stream, router).await,
    _ => serve_http1(tls_stream, router).await,
}
```

## 🎯 KUBERNETES INTEGRATION

### Watch Ingress Resources (Stage 3 - Week 4-5)

```rust
use kube::{Api, Client, runtime::watcher};
use k8s_openapi::api::networking::v1::Ingress;

async fn watch_ingress(router: Arc<Router>) -> Result<()> {
    let client = Client::try_default().await?;
    let ingresses: Api<Ingress> = Api::all(client);

    let watcher = watcher(ingresses, watcher::Config::default());

    tokio::pin!(watcher);
    while let Some(event) = watcher.try_next().await? {
        match event {
            watcher::Event::Applied(ing) => {
                sync_ingress_to_routes(&ing, &router).await?;
            }
            watcher::Event::Deleted(ing) => {
                remove_ingress_routes(&ing, &router).await?;
            }
            _ => {}
        }
    }

    Ok(())
}
```

### EndpointSlices for Backend Discovery

```rust
use k8s_openapi::api::discovery::v1::EndpointSlice;

async fn watch_endpoint_slices(router: Arc<Router>) -> Result<()> {
    let client = Client::try_default().await?;
    let endpoints: Api<EndpointSlice> = Api::all(client);

    // Event-driven updates (NOT 10-second polling!)
    let watcher = watcher(endpoints, watcher::Config::default());

    tokio::pin!(watcher);
    while let Some(event) = watcher.try_next().await? {
        match event {
            watcher::Event::Applied(eps) => {
                update_backends_immediately(&eps, &router).await?;
            }
            _ => {}
        }
    }

    Ok(())
}
```

**Why EndpointSlices?**
- Scales to 10K+ pods per service
- Lower API server load
- Topology-aware routing (same-zone preference)

### Sync Ingress → Routes

```rust
async fn sync_ingress_to_routes(ing: &Ingress, router: &Router) -> Result<()> {
    let rules = ing.spec.as_ref()
        .and_then(|s| s.rules.as_ref())
        .ok_or(Error::NoRules)?;

    for rule in rules {
        let host = rule.host.as_deref().unwrap_or("*");
        let paths = rule.http.as_ref()
            .and_then(|h| h.paths.as_ref())
            .ok_or(Error::NoPaths)?;

        for path in paths {
            // Check for gRPC annotation
            let protocol = ing.metadata.annotations.as_ref()
                .and_then(|a| a.get("rauta.io/backend-protocol"))
                .map(|s| s.as_str())
                .unwrap_or("HTTP");

            let route = Route {
                method: HttpMethod::ALL,
                path: path.path.clone().unwrap_or_else(|| "/".into()),
                host: host.into(),
                protocol: match protocol {
                    "GRPC" | "GRPCS" => Protocol::Grpc,
                    "HTTP2" => Protocol::Http2,
                    _ => Protocol::Http1,
                },
                backends: resolve_endpoints(&path.backend).await?,
            };

            router.add_route(route).await?;
        }
    }

    Ok(())
}
```

## 📊 eBPF OBSERVABILITY

### What eBPF Does (NOT L7 Routing!)

**eBPF Role**: Connection tracking and metrics collection

```c
// rauta-observer.bpf.c
struct route_metrics {
    u64 requests;
    u64 bytes_sent;
    u64 bytes_received;
    u64 latency_sum_us;
    u64 errors;
};

// Per-route metrics map
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_HASH);
    __uint(max_entries, 10000);
    __type(key, u64);    // route_hash
    __type(value, struct route_metrics);
} ROUTE_METRICS SEC(".maps");

// Attach to kprobe:tcp_sendmsg
SEC("kprobe/tcp_sendmsg")
int tcp_send_probe(struct pt_regs *ctx) {
    // Track bytes sent per connection
    // Update route metrics based on connection → route mapping
    return 0;
}
```

**What We Track**:
- Connection state (SYN, ESTABLISHED, FIN)
- Request latency (connection open → first byte received)
- Bytes sent/received per route
- Error rates (connection failures, timeouts)
- Pod-level request distribution

**What We DON'T Track in eBPF**:
- ❌ HTTP method/path parsing (that's userspace!)
- ❌ HTTP/2 frame parsing (impossible in eBPF)
- ❌ TLS decryption (userspace only)

### Metrics Export (Prometheus)

```rust
// Export eBPF metrics via Prometheus
#[tokio::main]
async fn metrics_exporter(bpf: Arc<Bpf>) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        interval.tick().await;

        // Read per-CPU metrics and aggregate
        let mut metrics: PerCpuArray<_, RouteMetrics> =
            PerCpuArray::try_from(bpf.map_mut("ROUTE_METRICS")?)?;

        for (route_id, per_cpu_values) in metrics.iter() {
            let total = aggregate_per_cpu(per_cpu_values);

            // Export to Prometheus
            REQUESTS_TOTAL
                .with_label_values(&[&route_path, &route_method])
                .set(total.requests);

            LATENCY_SECONDS
                .with_label_values(&[&route_path])
                .observe(total.latency_sum_us as f64 / 1_000_000.0);
        }
    }
}
```

**Prometheus Metrics**:
```
# Request counts
rauta_requests_total{route="/api/users",method="GET"} 1000000

# Latency histogram
rauta_request_duration_seconds_bucket{route="/api/users",le="0.005"} 950000
rauta_request_duration_seconds_bucket{route="/api/users",le="0.010"} 990000

# Backend health
rauta_backend_requests_total{pod="user-svc-abc123",zone="us-east-1a"} 333333

# Cache hit rate (future: if we add L4 fast path)
rauta_route_cache_hit_rate{route="/api/users"} 0.99
```

## 🧪 TESTING STRATEGY

### Unit Tests (Rust)

```rust
#[tokio::test]
async fn test_http2_request_routing() {
    let router = Router::new();

    let backends = vec![Backend::new(
        u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100
    )];
    router.add_route(HttpMethod::GET, "/api/users", backends).unwrap();

    // Simulate HTTP/2 request
    let req = Request::builder()
        .version(Version::HTTP_2)
        .uri("/api/users")
        .body(Body::empty())
        .unwrap();

    let backend = router.select_backend(&req).await.unwrap();
    assert_eq!(Ipv4Addr::from(backend.ipv4), Ipv4Addr::new(10, 0, 1, 1));
}
```

### Integration Tests (Real Traffic)

```bash
# Start test backend
cargo run --bin test-backend &

# Start RAUTA proxy
cargo run --bin control &

# Test HTTP/1.1
curl http://localhost:8080/api/users
# Should route to backend

# Test HTTP/2
curl --http2-prior-knowledge http://localhost:8080/api/users
# Should route to backend via HTTP/2

# Test gRPC
grpcurl -plaintext localhost:8080 user.UserService/GetUser
# Should route gRPC request
```

### Load Testing

```bash
# HTTP/1.1 load test
wrk -t12 -c400 -d30s http://localhost:8080/api/test

# HTTP/2 load test
h2load -n100000 -c100 -m10 http://localhost:8080/api/test

# gRPC load test
ghz --insecure --proto user.proto --call user.UserService/GetUser -n 100000 localhost:8080
```

**Performance Targets**:
- HTTP/1.1: 100K+ rps
- HTTP/2: 200K+ rps (multiplexing)
- gRPC: 50K+ rps (larger payloads)
- p99 latency: <5ms

## 🚀 DEPLOYMENT

### DaemonSet (One per Node)

```yaml
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: rauta
  namespace: kube-system
spec:
  selector:
    matchLabels:
      app: rauta
  template:
    spec:
      hostNetwork: true
      containers:
      - name: rauta
        image: rauta:v0.1.0
        securityContext:
          capabilities:
            add: ["NET_BIND_SERVICE", "BPF", "PERFMON"]
        ports:
        - containerPort: 80
          protocol: TCP
        - containerPort: 443
          protocol: TCP
        - containerPort: 9090  # Prometheus metrics
          protocol: TCP
        env:
        - name: RAUTA_LOG_LEVEL
          value: "info"
        - name: RAUTA_ENABLE_OBSERVABILITY
          value: "true"
```

## 📚 LEARNING RESOURCES

### Must Read

1. **Cilium Architecture**: https://docs.cilium.io/en/stable/concepts/ebpf/
   - How eBPF redirects to Envoy for L7
2. **NGINX Ingress Controller**: https://kubernetes.github.io/ingress-nginx/
   - K8s integration patterns
3. **Envoy Proxy Architecture**: https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/arch_overview
   - L7 routing patterns
4. **Aya Book**: https://aya-rs.dev/book/
   - Rust eBPF framework

### Code References

- **Cilium**: https://github.com/cilium/cilium (eBPF + Envoy integration)
- **NGINX Ingress**: https://github.com/kubernetes/ingress-nginx (K8s patterns)
- **Linkerd**: https://github.com/linkerd/linkerd2-proxy (Rust L7 proxy)

## ⚠️ REALISTIC SCOPE

### What RAUTA IS

✅ **Production-ready K8s ingress controller**
- Full HTTP/1.1, HTTP/2, gRPC support
- TLS termination with SNI
- K8s-native (Ingress, Gateway API)
- Deep observability via eBPF

✅ **Learning project for Rust + eBPF**
- Explore Rust async I/O patterns
- Learn eBPF observability techniques
- Build production-quality Rust systems

### What RAUTA IS NOT

❌ **Not an XDP HTTP parser**
- Parsing HTTP/2 in XDP is unrealistic
- Modern protocols (gRPC, HTTP/2) require userspace

❌ **Not trying to beat Katran**
- Katran does L4 (10M pps)
- RAUTA does L7 (100K rps)
- Different problem spaces

❌ **Not replacing Envoy/NGINX**
- Those are mature, battle-tested
- RAUTA is a learning project that may become production-ready

## 🎖️ DEFINITION OF DONE

A feature is complete when:

- [ ] Design documented in `docs/`
- [ ] Rust tests passing (unit + integration)
- [ ] HTTP/1.1, HTTP/2, or gRPC support working
- [ ] Load test meets performance targets
- [ ] eBPF observability integrated (if applicable)
- [ ] K8s integration tested (if applicable)
- [ ] Documentation updated

**NO STUBS. NO TODOs. COMPLETE CODE OR NOTHING.**

## 🏆 FINAL MANIFESTO

**RAUTA is a production-focused learning project for building modern K8s infrastructure in Rust.**

We're building:
- ✅ Real-world K8s ingress controller (HTTP/1.1, HTTP/2, gRPC)
- ✅ Deep observability via eBPF (without trying to do L7 in kernel)
- ✅ K8s-native integration (Ingress, EndpointSlices, Gateway API)
- ✅ Production-quality Rust (TDD, strong typing, zero unsafe)

We're NOT building:
- ❌ XDP HTTP parser (unrealistic for modern protocols)
- ❌ Another Envoy clone (learn from Envoy, don't copy it)
- ❌ Experimental dead-end (focus on production viability)

**Learn. Build. Ship. 🦀⚡**

---

**Finnish Tool Ecosystem**:
- **URPO**: Trace explorer 🔍
- **TAPIO**: K8s observer 🌲
- **AHTI**: Correlation engine 🌊
- **ELAVA**: AWS scanner 💚
- **RAUTA**: Ingress controller ⚙️ (iron)

**Iron-clad routing with deep visibility.**
