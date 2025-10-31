# RAUTA: Kubernetes Gateway API Controller + eBPF Observability

**RAUTA = Iron-clad routing with deep visibility** ⚡🦀

---

## ⚠️ CRITICAL: Project Nature

**THIS IS A PRODUCTION-FOCUSED LEARNING PROJECT**
- **Goal**: Build a production-ready K8s Gateway API controller with unique eBPF observability
- **Language**: 100% Rust (userspace) + eBPF (observability)
- **Status**: 🚧 Stage 1 COMPLETE ✅ | Stage 2 NEXT
- **Approach**: Ship working software, learn by building, validate with research

---

## 🎯 PROJECT MISSION

**Mission**: Build the fastest, most observable Kubernetes Gateway API controller using Rust + eBPF

**The Three Use Cases:**

1. **Ingress Controller** (Stage 1 - DONE ✅)
   - Modern Gateway API native L7 routing
   - Maglev load balancing, Prometheus metrics
   - Validated in kind cluster

2. **Observability Layer** (Stage 2 - NEXT)
   - Deep HTTP insights via eBPF (XDP observation mode)
   - Detect slow responses, 5xx spikes, probe failures
   - Complements TAPIO in false-systems platform

3. **Service Mesh Without Sidecars** (Stage 3 - FUTURE)
   - Transparent proxy using eBPF sockops
   - 30% latency reduction (Cilium validates)
   - No sidecar overhead

**Core Philosophy:**
- **eBPF captures, userspace parses** (Brendan Gregg principle)
- **Rust for safety and performance** (Cloudflare Pingora validates this)
- **Gateway API native** (no legacy Ingress baggage)
- **Research-backed decisions** (HTTP/2 > HTTP/3, io_uring, skip DPDK)

---

## 🏗️ ARCHITECTURE PHILOSOPHY

### The Consolidated Vision

**RAUTA combines three capabilities:**

```
┌────────────────────────────────────────────────────────┐
│                RAUTA Architecture                       │
├────────────────────────────────────────────────────────┤
│                                                        │
│  Observability Layer (eBPF - Stage 2)                  │
│  ┌──────────────────────────────────────────────────┐ │
│  │  XDP Program (observation mode)                  │ │
│  │  - Parse HTTP requests/responses                 │ │
│  │  - Track latency, errors                         │ │
│  │  - Send to ring buffer                           │ │
│  │  - ALWAYS return XDP_PASS ✅                     │ │
│  └────────────────┬─────────────────────────────────┘ │
│                   │ Ring Buffer                        │
│                   ▼                                    │
│  ┌──────────────────────────────────────────────────┐ │
│  │  Processors (TAPIO pattern)                      │ │
│  │  - SlowResponseProcessor                         │ │
│  │  - ErrorRateProcessor                            │ │
│  │  - HealthCheckProcessor                          │ │
│  └────────────────┬─────────────────────────────────┘ │
│                   │                                    │
│                   ▼                                    │
│  Control + Data Plane (Rust - Stage 1 DONE ✅)        │
│  ┌──────────────────────────────────────────────────┐ │
│  │  Gateway API Controllers                         │ │
│  │  - GatewayClass, Gateway, HTTPRoute              │ │
│  │  - Service → EndpointSlice resolver              │ │
│  │                                                  │ │
│  │  HTTP Proxy (tokio + hyper)                      │ │
│  │  - HTTP/1.1, HTTP/2 (future)                     │ │
│  │  - Maglev load balancing                         │ │
│  │  - TLS termination (future)                      │ │
│  │                                                  │ │
│  │  Observability                                   │ │
│  │  - Prometheus metrics                            │ │
│  │  - OTLP traces (future)                          │ │
│  └──────────────────────────────────────────────────┘ │
│                                                        │
└────────────────────────────────────────────────────────┘
```

**Why This Architecture:**
- **XDP observability** - Sees packets BEFORE kernel TCP stack (true latency)
- **Userspace L7** - Full HTTP/2, TLS, complex routing
- **Gateway API** - Modern K8s standard (not legacy Ingress)
- **false-systems integration** - RAUTA → TAPIO → AHTI → URPO

---

## 🦀 RUST + AYA REQUIREMENTS

### Language Requirements
- **THIS IS A RUST PROJECT** - All userspace code in Rust
- **eBPF Code**: Aya-RS (Rust eBPF framework)
- **NO GO CODE** - Unlike Cilium's Go control plane, we use Rust everywhere
- **STRONG TYPING ONLY** - No `Box<dyn Any>` or runtime type checking

### Aya Framework Patterns

```rust
// ✅ Load XDP program for observation (Stage 2)
use aya::{Bpf, programs::Xdp};

let mut bpf = Bpf::load(include_bytes_aligned!(
    "../../target/bpfel-unknown-none/release/rauta-observer"
))?;

let program: &mut Xdp = bpf.program_mut("rauta_observe")?.try_into()?;
program.load()?;
program.attach("eth0", XdpFlags::SKB_MODE)?;  // Observation mode

// ✅ Access ring buffer events
use aya::maps::RingBuf;

let mut ring_buf = RingBuf::try_from(bpf.map_mut("EVENTS")?)?;

while let Some(event) = ring_buf.next() {
    let http_event: HttpRequestEvent = unsafe {
        std::ptr::read(event.as_ptr() as *const HttpRequestEvent)
    };

    // Process event in Rust (not eBPF!)
    process_http_event(http_event).await;
}
```

**NO STUBS. NO TODOs. COMPLETE CODE ONLY.**

---

## 🧪 TDD Workflow (RED → GREEN → REFACTOR)

**MANDATORY**: All code must follow strict Test-Driven Development

### RED Phase: Write Failing Tests First

```rust
// Step 1: Write test that FAILS (RED)
#[tokio::test]
async fn test_gateway_route_idempotency() {
    let router = Router::new();

    let backends = vec![Backend::new(
        u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100
    )];

    // Add route first time
    router.add_route(HttpMethod::GET, "/api/users", backends.clone())
        .expect("First add should succeed");

    // Add same route again (should be idempotent)
    router.add_route(HttpMethod::GET, "/api/users", backends.clone())
        .expect("Second add should succeed (idempotent)");

    // Verify route still works
    let route_match = router
        .select_backend(HttpMethod::GET, "/api/users", None, None)
        .expect("Should find backend after duplicate add");

    assert_eq!(
        Ipv4Addr::from(route_match.backend.ipv4),
        Ipv4Addr::new(10, 0, 1, 1)
    );
}

// Step 2: Verify test FAILS
// $ cargo test
// # test_gateway_route_idempotency ... FAILED ✅ RED phase confirmed
```

### GREEN Phase: Minimal Implementation

```rust
// Step 3: Write MINIMAL code to pass test
impl Router {
    pub fn add_route(
        &self,
        method: HttpMethod,
        path: &str,
        backends: Vec<Backend>,
    ) -> Result<(), String> {
        let path_hash = fnv1a_hash(path.as_bytes());
        let key = RouteKey { method, path_hash };

        // Check if route exists with same backends (idempotent fast path)
        {
            let routes = self.routes.read().unwrap();
            if let Some(existing) = routes.get(&key) {
                if existing.backends == backends {
                    return Ok(());  // No-op if identical
                }
            }
        }

        // Build Maglev table and insert
        let maglev_table = maglev_build_compact_table(&backends);
        let route = Route {
            pattern: Arc::from(path),
            backends,
            maglev_table,
        };

        let mut routes = self.routes.write().unwrap();
        routes.insert(key, route);

        // Rebuild matchit router (doesn't support updates)
        // ... rebuild logic

        Ok(())
    }
}

// Step 4: Verify tests PASS
// $ cargo test
// # test_gateway_route_idempotency ... ok ✅ GREEN phase confirmed
```

### REFACTOR Phase: Improve Code Quality

```rust
// Step 5: Add edge cases
#[tokio::test]
async fn test_router_update_route_backends() {
    // Test updating backends for existing route
}

// Step 6: Refactor for better design
impl Router {
    fn rebuild_prefix_router(&self, routes: &HashMap<RouteKey, Route>) -> Result<matchit::Router<RouteKey>, String> {
        let mut new_router = matchit::Router::new();

        for (route_key, route) in routes.iter() {
            let path_str = route.pattern.as_ref();
            new_router.insert(path_str.to_string(), *route_key)?;

            let prefix_pattern = if path_str == "/" {
                "/{*rest}".to_string()
            } else {
                format!("{}/{{*rest}}", path_str.trim_end_matches('/'))
            };
            new_router.insert(prefix_pattern, *route_key)?;
        }

        Ok(new_router)
    }
}

// Step 7: Verify tests still PASS after refactor
// $ cargo test
// # All tests ... ok ✅ REFACTOR complete
```

### TDD Checklist

- [ ] **RED**: Write failing test first
- [ ] **RED**: Verify compilation fails or test fails
- [ ] **GREEN**: Write minimal implementation
- [ ] **GREEN**: Verify all tests pass
- [ ] **REFACTOR**: Add edge cases, improve design
- [ ] **REFACTOR**: Verify tests still pass
- [ ] **Commit**: `git add . && git commit -m "feat: ..."` (incremental commits)

**Example Session:**
```bash
# Router idempotency (TDD - 3 commits)
1. RED:   Write test_gateway_route_idempotency → FAIL ✅
2. GREEN: Implement idempotent add_route → PASS ✅
3. COMMIT: git commit -m "feat: add router idempotency for K8s reconciliation"

# Add update test (TDD - 2 commits)
1. RED:   Write test_router_update_route_backends → FAIL ✅
2. GREEN: Fix backend comparison logic → PASS ✅
3. COMMIT: git commit -m "fix: handle backend updates in router"
```

---

## 🔥 PERFORMANCE REQUIREMENTS

### Target: Production-Ready L7 Performance

**Current (Stage 1):**
- ✅ ~10,000 rps baseline (validated)
- ✅ ~55ns metrics overhead (0.0016% of latency)
- ✅ Maglev O(1) backend selection

**Research-Backed Roadmap:**

From **cutting-edge-research-2024-2025.md**:

**Stage 2: HTTP/2 Support**
- ✅ Stick with HTTP/2 (ACM 2024: HTTP/3 is 45% slower!)
- ✅ Multiplexing, header compression
- Expected: 5x throughput → 50,000 rps

**Stage 3: io_uring Backend**
- ✅ Zero-copy splice/sendfile (Kernel Recipes 2024)
- ✅ +31-43% throughput improvement → 70,000+ rps
- Linux 5.7+ requirement (acceptable)
- Implementation: tokio-uring crate

**Stage 4: eBPF Sockmap** (Service Mesh Mode)
- ✅ 30% latency reduction (Cilium validates)
- ✅ Bypass kernel TCP stack for pod-to-pod
- Used by: Cilium, Istio in production

**Decisions NOT to Implement:**
- ❌ HTTP/3: Research proves it's 45% slower than HTTP/2 (ACM 2024)
- ❌ DPDK: L7 bottleneck is HTTP parsing, not network (F-Stack research)

**Expected Final Performance:**
```
Baseline (Stage 1):     ~10,000 rps
+ Streaming:            → 50,000 rps
+ HTTP/2:               → 100,000 rps
+ io_uring:             → 140,000 rps
+ eBPF sockmap:         → 180,000 rps (service mesh mode)
```

### Performance Patterns

```rust
// ✅ Zero-allocation strings (Arc<str>)
struct Route {
    pattern: Arc<str>,  // 5ns clone vs 50ns for String
    backends: Vec<Backend>,
    maglev_table: Vec<u8>,
}

// ✅ Static string helpers
fn method_to_str(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::GET => "GET",
        HttpMethod::POST => "POST",
        // No allocations!
    }
}

// ✅ Cardinality control for metrics
HTTP_REQUESTS_TOTAL
    .with_label_values(&[method_str, route.pattern.as_ref(), status_str])
    .inc();  // Use pattern, not actual path!

// ✅ Early return for /metrics endpoint
if path == "/metrics" && *req.method() == hyper::Method::GET {
    return serve_metrics().await;  // Don't record metrics for metrics endpoint
}
```

---

## 🎯 KUBERNETES INTEGRATION

### Gateway API v1 (Stage 1 - DONE ✅)

```rust
use gateway_api::apis::standard::{
    gateway_classes::GatewayClass,
    gateways::Gateway,
    http_routes::HTTPRoute,
};
use kube::runtime::{controller, watcher};

// GatewayClass reconciler
async fn reconcile_gateway_class(gc: Arc<GatewayClass>, ctx: Arc<Context>) -> Result<Action> {
    // Accept if controllerName == "rauta.io/gateway-controller"
    if gc.spec.controller_name == "rauta.io/gateway-controller" {
        update_status(gc, Condition::Accepted(true)).await?;
    }
    Ok(Action::requeue(Duration::from_secs(300)))
}

// HTTPRoute reconciler
async fn reconcile_http_route(route: Arc<HTTPRoute>, ctx: Arc<Context>) -> Result<Action> {
    // Parse routing rules
    for rule in route.spec.rules {
        let matches = rule.matches;  // PathPrefix, Headers, etc.
        let backend_refs = rule.backend_refs;  // Services

        // Add route to router (idempotent!)
        ctx.router.add_route(
            HttpMethod::GET,  // HTTPRoute doesn't specify method
            &matches[0].path.value,
            resolve_backends(&backend_refs).await?,
        )?;
    }

    // Update status
    update_route_status(route, Condition::Accepted(true)).await?;
    Ok(Action::requeue(Duration::from_secs(300)))
}
```

**Status**: ✅ WORKING
- GatewayClass, Gateway, HTTPRoute controllers implemented
- Router idempotency handles reconciliation loops
- Validated in kind cluster

**Next**: Service → EndpointSlice resolution (dynamic backends)

---

## 📊 eBPF OBSERVABILITY (Stage 2 - NEXT)

### XDP Observer Pattern (Brendan Gregg Approach)

**eBPF Role**: Capture HTTP events, NOT route packets

```rust
// XDP program (observation mode)
#[xdp]
fn rauta_observe(ctx: XdpContext) -> u32 {
    // Parse ethernet → IP → TCP
    let eth = parse_eth(&ctx)?;
    let ip = parse_ipv4(&ctx, eth)?;
    let tcp = parse_tcp(&ctx, ip)?;

    // Parse HTTP request (if present)
    let http_start = tcp_payload_start(&tcp);
    if let Ok(request) = parse_http_request(&ctx, http_start) {
        let evt = HttpRequestEvent {
            timestamp: bpf_ktime_get_ns(),
            client_ip: ip.saddr,
            server_ip: ip.daddr,
            server_port: tcp.dest,
            method: request.method,
            path_hash: fnv1a_hash(&request.path),
            path: request.path,  // Up to 256 bytes
        };

        // Send to ring buffer (non-blocking)
        EVENTS.output(&ctx, &evt, 0);
    }

    // ALWAYS pass packet to kernel (observation only!)
    XDP_PASS
}
```

### Processor Pattern (TAPIO Architecture)

```rust
// SlowResponseProcessor (userspace Rust)
pub struct SlowResponseProcessor {
    baseline_store: Arc<BaselineStore>,
}

impl HttpProcessor for SlowResponseProcessor {
    fn process(
        &self,
        ctx: &ProcessorContext,
        req: &HttpRequestEvent,
        resp: Option<&HttpResponseEvent>,
    ) -> Option<ObserverEvent> {
        let resp = resp?;

        // Calculate latency
        let latency_ns = resp.timestamp - req.timestamp;
        let latency_ms = (latency_ns as f64) / 1_000_000.0;

        // Lookup baseline p99 for this service
        let service_key = (req.server_ip, req.server_port);
        let baseline = self.baseline_store.get_p99(service_key)?;

        // Detect slow response (p99 > 2x baseline)
        if latency_ms > baseline * 2.0 {
            Some(ObserverEvent {
                type_: "http_latency".into(),
                subtype: "slow_response".into(),
                http_data: Some(HttpEventData {
                    method: req.method.to_string(),
                    path: String::from_utf8_lossy(&req.path).into(),
                    status_code: resp.status_code,
                    latency_ms,
                    server_ip: ipv4_to_string(req.server_ip),
                    server_port: req.server_port,
                    // K8s enrichment added later
                    pod_name: None,
                    namespace: None,
                }),
                k8s_context: None,
            })
        } else {
            None
        }
    }
}
```

**Status**: Design complete (3-month MVP planned)
- TAPIO processor pattern proven
- Brendan Gregg patterns documented
- Ready for implementation

---

## 🧪 TESTING STRATEGY

### Unit Tests (Rust)

```rust
#[tokio::test]
async fn test_router_maglev_distribution() {
    let router = Router::new();

    let backends = vec![
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 3)), 8080, 100),
    ];

    router.add_route(HttpMethod::GET, "/api/users", backends).unwrap();

    // Simulate 10K requests
    let mut distribution = HashMap::new();
    for i in 0..10_000 {
        let src_ip = Some(0x0100007f + i);  // Vary client IP
        let src_port = Some((i % 65535) as u16);

        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users", src_ip, src_port)
            .unwrap();

        *distribution.entry(route_match.backend.ipv4).or_insert(0) += 1;
    }

    // Each backend should get ~33% (within 5% variance)
    for count in distribution.values() {
        let percentage = (*count as f64) / 10_000.0;
        assert!((percentage - 0.33).abs() < 0.05);
    }
}
```

### Integration Tests (Kind Cluster)

```bash
# Create kind cluster
kind create cluster --name rauta-test

# Install Gateway API CRDs
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.0.0/standard-install.yaml

# Deploy RAUTA
kubectl apply -f deploy/rauta-daemonset.yaml

# Create test GatewayClass
kubectl apply -f - <<EOF
apiVersion: gateway.networking.k8s.io/v1
kind: GatewayClass
metadata:
  name: rauta
spec:
  controllerName: rauta.io/gateway-controller
EOF

# Verify GatewayClass accepted
kubectl get gatewayclass rauta -o yaml
# Should show: status.conditions[type=Accepted].status=True

# Create Gateway + HTTPRoute
kubectl apply -f examples/gateway-httproute.yaml

# Test routing
curl http://$(kubectl get svc rauta -o jsonpath='{.status.loadBalancer.ingress[0].ip}')/api/test
```

### Load Testing

```bash
# HTTP/1.1 load test
wrk -t12 -c400 -d30s http://rauta-lb/api/test

Expected:
  Requests/sec: 10,000+
  Latency p99: <50ms
  Memory: <100MB

# Check metrics
curl http://rauta-lb:9090/metrics | grep rauta_requests_total
```

---

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
      hostNetwork: true  # Access node network
      containers:
      - name: rauta
        image: rauta:v0.1.0
        securityContext:
          capabilities:
            add: ["NET_BIND_SERVICE"]  # For ports 80/443
        ports:
        - containerPort: 80
        - containerPort: 443
        - containerPort: 9090  # Prometheus metrics
        env:
        - name: RAUTA_MODE
          value: "ingress"  # Or "observability", "mesh"
        - name: RAUTA_GATEWAY_CLASS
          value: "rauta"
        - name: RAUTA_LOG_LEVEL
          value: "info"
```

### Observability Mode (Stage 2)

```yaml
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: rauta-observer
spec:
  template:
    spec:
      hostNetwork: true
      containers:
      - name: rauta
        image: rauta:v0.2.0
        securityContext:
          privileged: true  # Required for XDP
          capabilities:
            add: ["NET_ADMIN", "SYS_ADMIN", "BPF"]
        env:
        - name: RAUTA_MODE
          value: "observability"
        - name: RAUTA_INTERFACE
          value: "eth0"
        - name: OTEL_EXPORTER_OTLP_ENDPOINT
          value: "http://otel-collector:4318"
```

---

## 📚 LEARNING RESOURCES

### Must Read

1. **BPF Performance Tools** (Brendan Gregg) - Chapter 10: Networking
2. **Cilium Architecture**: https://docs.cilium.io/en/stable/concepts/ebpf/
3. **Gateway API**: https://gateway-api.sigs.k8s.io/
4. **Aya Book**: https://aya-rs.dev/book/

### Code References

- **Cloudflare Pingora**: Rust proxy (validates architecture)
- **TAPIO**: `/Users/yair/projects/tapio/` (processor pattern)
- **Cilium**: eBPF + Envoy integration patterns

### Research Papers

- **ACM 2024**: "QUIC is not Quick Enough" (HTTP/3 is 45% slower)
- **Kernel Recipes 2024**: "io_uring zero-copy" (+31-43% throughput)
- **Google**: "Maglev: A Fast and Reliable Software Network Load Balancer"

---

## ⚠️ REALISTIC SCOPE

### What RAUTA IS

✅ **Production-ready Gateway API controller**
- Full Gateway API v1 support (GatewayClass, Gateway, HTTPRoute)
- Maglev load balancing
- Prometheus observability
- Validated in kind cluster

✅ **Learning project for Rust + eBPF**
- Explore Rust async patterns (tokio, hyper)
- Learn eBPF observability (XDP observation, not routing)
- Build production-quality systems

✅ **false-systems platform component**
- RAUTA (HTTP ingress) → TAPIO (pod metrics) → AHTI (correlation) → URPO (UI)

### What RAUTA IS NOT

❌ **Not an XDP HTTP router**
- XDP sees raw packets (no TCP reassembly)
- Modern protocols (HTTP/2, TLS) require userspace
- eBPF is for observation, NOT routing

❌ **Not trying to beat Katran**
- Katran does L4 (10M pps)
- RAUTA does L7 (100K+ rps)
- Different problem spaces

❌ **Not replacing Envoy/NGINX**
- Those are mature, battle-tested
- RAUTA is a focused tool with unique strengths

---

## 🎖️ DEFINITION OF DONE

A feature is complete when:

- [ ] Design documented in `docs/`
- [ ] Rust tests passing (unit + integration)
- [ ] TDD workflow followed (RED → GREEN → REFACTOR)
- [ ] Load test meets performance targets
- [ ] K8s integration tested (if applicable)
- [ ] Documentation updated
- [ ] Code reviewed (pre-commit hooks pass)

**NO STUBS. NO TODOs. COMPLETE CODE OR NOTHING.**

---

## 🏆 FINAL MANIFESTO

**RAUTA is a production-focused learning project for building modern K8s infrastructure in Rust.**

**We're building:**
- ✅ Real-world Gateway API controller (working in kind cluster)
- ✅ Deep observability via eBPF (without trying to do L7 in kernel)
- ✅ Research-backed architecture (Cloudflare, Cilium, Brendan Gregg)
- ✅ Production-quality Rust (TDD, strong typing, pre-commit hooks)

**We're NOT building:**
- ❌ XDP HTTP parser (unrealistic for HTTP/2, TLS, gRPC)
- ❌ Another Envoy clone (learn from Envoy, don't copy it)
- ❌ Experimental dead-end (every feature has production use case)

**Competitive Positioning:**
- **vs Kong**: Native Rust (no LuaJIT overhead), 10-15x faster expected
- **vs Envoy**: Memory safe, zero-copy streaming, built-in eBPF observability
- **vs Cilium**: Works with any CNI (incremental adoption, not replacement)

**Unique Differentiators:**
1. **Gateway API native** - Modern standard from day 1
2. **Maglev hashing** - Google's algorithm (proven at scale)
3. **eBPF observability** - Kernel + userspace visibility
4. **Cloudflare-proven** - Pingora validates Rust architecture
5. **Research-backed** - ACM papers, not hype (HTTP/2 > HTTP/3)

**Learn. Build. Ship. 🦀⚡**

---

**Finnish Tool Ecosystem:**
- **URPO**: Trace explorer 🔍
- **TAPIO**: K8s observer 🌲
- **AHTI**: Correlation engine 🌊
- **ELAVA**: AWS scanner 💚
- **RAUTA**: Ingress controller ⚙️ (iron)

**Iron-clad routing with deep visibility.**
