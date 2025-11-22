# RAUTA: Kubernetes Gateway API Controller with WASM Plugins

**RAUTA = Iron-clad routing with safe extensibility**

---

## CRITICAL: Project Nature

**THIS IS A PRODUCTION-FOCUSED LEARNING PROJECT**
- **Goal**: Build a production-ready K8s Gateway API controller with WASM plugin extensibility
- **Language**: 100% Rust (userspace) + WASM (plugins)
- **Status**: Stage 1 COMPLETE (Gateway API controller) | Stage 2 NEXT (WASM plugins)
- **Approach**: Ship working software, learn by building, validate with research

---

## PROJECT MISSION

**Mission**: Build a Kubernetes Gateway API controller that developers can safely extend with WASM plugins

**Core Value Proposition:**

**"Gateway API routing you can extend with WASM plugins - like Kong, but safer"**

**The Differentiator: WASM Plugins**
- Kong uses Lua plugins (can crash the gateway)
- Envoy uses C++ filters (dangerous to write, requires recompilation)
- RAUTA uses WASM plugins (sandboxed, multi-language, hot-reload)

**Why This Matters:**
- Write plugins in ANY language (Rust, Go, TypeScript, C++)
- Cannot crash the gateway (sandboxed execution)
- Hot-reload without downtime
- Built-in rate limiting (CPU, memory per plugin)

**Core Philosophy:**
- **Rust for safety and performance** (Cloudflare Pingora validates this)
- **Gateway API native** (no legacy Ingress baggage)
- **WASM for extensibility** (safe, fast, multi-language)
- **Research-backed decisions** (HTTP/2 > HTTP/3, skip DPDK, skip io_uring for L7)

---

## ARCHITECTURE PHILOSOPHY

### The Vision

**RAUTA is a Gateway API controller with three layers:**

```
┌────────────────────────────────────────────────────────┐
│                RAUTA Architecture                       │
├────────────────────────────────────────────────────────┤
│                                                        │
│  WASM Plugin Layer (Stage 2 - NEXT)                    │
│  ┌──────────────────────────────────────────────────┐ │
│  │  Plugin Runtime (wasmtime)                       │ │
│  │  - Request/Response hooks                        │ │
│  │  - Plugin SDK (Rust, Go, TypeScript)             │ │
│  │  - Hot-reload support                            │ │
│  │  - CPU/memory limits per plugin                  │ │
│  │                                                  │ │
│  │  Built-in Plugins                                │ │
│  │  - JWT authentication                            │ │
│  │  - CORS handling                                 │ │
│  │  - Rate limiting                                 │ │
│  │  - Request transformation                        │ │
│  └──────────────────────────────────────────────────┘ │
│                                                        │
│  Gateway Core (Rust - Stage 1 DONE)                    │
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
- **Userspace L7** - Full HTTP/2, TLS, complex routing (not eBPF)
- **WASM plugins** - Safe extensibility without gateway crashes
- **Gateway API** - Modern K8s standard (not legacy Ingress)
- **Rust core** - Memory safety, performance, zero-cost abstractions

**What We Learned from "Learning eBPF" (Liz Rice):**
- XDP operates on raw packets BEFORE TCP reassembly
- XDP cannot parse HTTP/2, TLS, or complex protocols
- Even Cilium uses Envoy for L7 HTTP parsing (not eBPF)
- eBPF is excellent for L3/L4, NOT for L7 application logic
- Conclusion: HTTP routing belongs in userspace, NOT in XDP

---

## RUST + WASM REQUIREMENTS

### Language Requirements
- **THIS IS A RUST PROJECT** - All userspace code in Rust
- **WASM for Plugins**: wasmtime runtime (Rust-based)
- **NO GO CODE** - Unlike Cilium's Go control plane, we use Rust everywhere
- **STRONG TYPING ONLY** - No `Box<dyn Any>` or runtime type checking

### WASM Plugin Pattern

```rust
// Loading and running WASM plugins
use wasmtime::*;

pub struct PluginRuntime {
    engine: Engine,
    plugins: HashMap<String, Plugin>,
}

pub struct Plugin {
    instance: Instance,
    on_request: TypedFunc<(u32, u32), u32>,  // (request_ptr, request_len) -> decision
    on_response: TypedFunc<(u32, u32), u32>,
}

impl PluginRuntime {
    pub async fn load_plugin(&mut self, name: &str, wasm_bytes: &[u8]) -> Result<()> {
        let module = Module::new(&self.engine, wasm_bytes)?;
        let mut store = Store::new(&self.engine, ());

        // Create instance with memory limits
        let instance = Instance::new(&mut store, &module, &[])?;

        // Get exported functions
        let on_request = instance
            .get_typed_func::<(u32, u32), u32>(&mut store, "on_request")?;
        let on_response = instance
            .get_typed_func::<(u32, u32), u32>(&mut store, "on_response")?;

        self.plugins.insert(name.to_string(), Plugin {
            instance,
            on_request,
            on_response,
        });

        Ok(())
    }

    pub async fn run_request_plugins(&self, req: &Request) -> PluginDecision {
        for plugin in self.plugins.values() {
            // Serialize request to plugin memory
            let req_bytes = serialize_request(req);

            // Call plugin (with timeout and resource limits)
            let decision = plugin.on_request.call(&mut store,
                (req_ptr, req_len))?;

            match decision {
                PLUGIN_CONTINUE => continue,
                PLUGIN_REJECT => return PluginDecision::Reject,
                PLUGIN_MODIFY => {
                    // Read modified request from plugin memory
                    let modified_req = deserialize_request(&plugin_memory);
                    *req = modified_req;
                }
            }
        }

        PluginDecision::Allow
    }
}
```

**NO STUBS. NO TODOs. COMPLETE CODE ONLY.**

---

## TDD Workflow (RED → GREEN → REFACTOR)

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
// # test_gateway_route_idempotency ... FAILED (RED phase confirmed)
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
// # test_gateway_route_idempotency ... ok (GREEN phase confirmed)
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
// # All tests ... ok (REFACTOR complete)
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
1. RED:   Write test_gateway_route_idempotency → FAIL
2. GREEN: Implement idempotent add_route → PASS
3. COMMIT: git commit -m "feat: add router idempotency for K8s reconciliation"

# Add update test (TDD - 2 commits)
1. RED:   Write test_router_update_route_backends → FAIL
2. GREEN: Fix backend comparison logic → PASS
3. COMMIT: git commit -m "fix: handle backend updates in router"
```

---

## PERFORMANCE REQUIREMENTS

### Target: Production-Ready L7 Performance

**Current (Stage 1):**
- ~10,000 rps baseline (validated)
- ~55ns metrics overhead (0.0016% of latency)
- Maglev O(1) backend selection

**Research-Backed Roadmap:**

From **cutting-edge-research-2024-2025.md**:

**Month 1-2: Production-Ready Gateway**
- EndpointSlice resolution (dynamic backends)
- HTTP/2 support (ACM 2024: HTTP/3 is 45% slower)
- TLS termination with rustls
- Basic rate limiting and circuit breakers
- DaemonSet deployment model
- Expected: 50,000+ rps

**Month 3-4: WASM Plugin System (THE DIFFERENTIATOR)**
- wasmtime runtime integration
- Plugin SDK (Rust, Go, TypeScript bindings)
- Built-in plugins (JWT, CORS, rate-limit)
- Hot-reload and resource limits
- Expected: 40,000+ rps (with plugins enabled)

**Month 5-6: HTTP/2 & Connection Pool Optimization**
- Connection pool tuning (max streams, idle timeout, pre-warming)
- HTTP/2 HPACK optimization (static table, Huffman encoding)
- Body streaming (zero-copy within userspace)
- Passive health checks (observe 5xx rates, circuit breakers)
- Expected: 70,000+ rps

**Decisions NOT to Implement:**
- **HTTP/3**: Research proves it's 45% slower than HTTP/2 (ACM 2024)
- **DPDK**: L7 bottleneck is HTTP parsing, not network (F-Stack research)
- **XDP HTTP parsing**: Impossible for HTTP/2, TLS (Learning eBPF validates)
- **io_uring**: Incompatible with hyper, 15% slower for HTTP (2024 benchmarks - see docs/research/IO_URING_ANALYSIS.md)

**Expected Final Performance:**
```
Baseline (Stage 1):       ~10,000 rps
+ HTTP/2:                 → 50,000 rps
+ WASM plugins:           → 40,000 rps (20% overhead acceptable)
+ Connection pool tuning: → 50,000 rps (1.25x over plugins)
+ HPACK optimization:     → 55,000 rps (1.10x)
+ Body streaming:         → 60,000 rps (1.09x)
────────────────────────────────────────
Target: 60,000+ rps (6x baseline, proven path)
```

### Performance Patterns

```rust
// Zero-allocation strings (Arc<str>)
struct Route {
    pattern: Arc<str>,  // 5ns clone vs 50ns for String
    backends: Vec<Backend>,
    maglev_table: Vec<u8>,
}

// Static string helpers
fn method_to_str(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::GET => "GET",
        HttpMethod::POST => "POST",
        // No allocations
    }
}

// Cardinality control for metrics
HTTP_REQUESTS_TOTAL
    .with_label_values(&[method_str, route.pattern.as_ref(), status_str])
    .inc();  // Use pattern, not actual path

// Early return for /metrics endpoint
if path == "/metrics" && *req.method() == hyper::Method::GET {
    return serve_metrics().await;  // Don't record metrics for metrics endpoint
}
```

---

## KUBERNETES INTEGRATION

### Gateway API v1 (Stage 1 - DONE)

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

        // Add route to router (idempotent)
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

**Status**: WORKING
- GatewayClass, Gateway, HTTPRoute controllers implemented
- Router idempotency handles reconciliation loops
- Validated in kind cluster

**Next**: Service → EndpointSlice resolution (dynamic backends)

---

## WASM PLUGIN SYSTEM (Stage 2 - NEXT)

### Plugin SDK Design

**Plugin Lifecycle Hooks:**
```rust
// Plugin interface (exported by WASM module)
#[no_mangle]
pub extern "C" fn on_request(req_ptr: *const u8, req_len: usize) -> u32 {
    // PLUGIN_CONTINUE = 0
    // PLUGIN_REJECT = 1
    // PLUGIN_MODIFY = 2
}

#[no_mangle]
pub extern "C" fn on_response(resp_ptr: *const u8, resp_len: usize) -> u32 {
    // Same return codes
}
```

**Example: JWT Authentication Plugin (Rust)**
```rust
use rauta_plugin_sdk::*;

#[rauta_plugin]
fn on_request(req: Request) -> PluginResult {
    // Extract Authorization header
    let auth_header = req.headers().get("Authorization")?;

    // Verify JWT
    let claims = verify_jwt(auth_header)?;

    // Add user ID to request context
    req.set_metadata("user_id", &claims.sub);

    PluginResult::Continue
}
```

**Example: Rate Limiting Plugin (Go)**
```go
package main

import "github.com/rauta/plugin-sdk-go"

//export on_request
func on_request(reqPtr *byte, reqLen int) int32 {
    req := plugin.DecodeRequest(reqPtr, reqLen)

    // Get client IP
    clientIP := req.Header("X-Forwarded-For")

    // Check rate limit
    if rateLimiter.IsAllowed(clientIP) {
        return plugin.CONTINUE
    }

    // Reject with 429 Too Many Requests
    req.SetResponse(429, "Rate limit exceeded")
    return plugin.REJECT
}
```

**Plugin Configuration (HTTPRoute annotation):**
```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: api-route
  annotations:
    rauta.io/plugins: |
      - name: jwt-auth
        config:
          secret: jwt-signing-key
          issuer: https://auth.example.com
      - name: rate-limit
        config:
          requests_per_minute: 100
          burst: 20
spec:
  parentRefs:
  - name: rauta-gateway
  rules:
  - matches:
    - path:
        type: PathPrefix
        value: /api
    backendRefs:
    - name: api-service
      port: 8080
```

---

## TESTING STRATEGY

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

### WASM Plugin Tests

```rust
#[tokio::test]
async fn test_jwt_plugin_rejects_invalid_token() {
    let runtime = PluginRuntime::new();
    runtime.load_plugin("jwt-auth", include_bytes!("plugins/jwt_auth.wasm")).await.unwrap();

    let mut req = Request::builder()
        .uri("/api/users")
        .header("Authorization", "Bearer invalid_token")
        .body(())
        .unwrap();

    let decision = runtime.run_request_plugins(&req).await;

    assert_eq!(decision, PluginDecision::Reject);
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

# Create Gateway + HTTPRoute with plugin
kubectl apply -f examples/gateway-httproute-with-plugins.yaml

# Test routing with JWT
curl -H "Authorization: Bearer $(generate_jwt)" \
  http://$(kubectl get svc rauta -o jsonpath='{.status.loadBalancer.ingress[0].ip}')/api/test
```

### Load Testing

```bash
# HTTP/1.1 load test
wrk -t12 -c400 -d30s http://rauta-lb/api/test

Expected:
  Requests/sec: 10,000+
  Latency p99: <50ms
  Memory: <100MB

# With WASM plugins enabled
wrk -t12 -c400 -d30s -H "Authorization: Bearer $JWT" http://rauta-lb/api/test

Expected:
  Requests/sec: 8,000+ (20% overhead acceptable)
  Latency p99: <60ms
  Memory: <150MB

# Check metrics
curl http://rauta-lb:9090/metrics | grep rauta_requests_total
```

---

## DEPLOYMENT

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
        - name: RAUTA_GATEWAY_CLASS
          value: "rauta"
        - name: RAUTA_LOG_LEVEL
          value: "info"
        - name: RAUTA_PLUGIN_DIR
          value: "/etc/rauta/plugins"
        volumeMounts:
        - name: plugin-config
          mountPath: /etc/rauta/plugins
      volumes:
      - name: plugin-config
        configMap:
          name: rauta-plugins
```

---

## LEARNING RESOURCES

### Must Read

1. **"Learning eBPF"** (Liz Rice) - Chapter 8: Networking
   - Key lesson: XDP cannot parse HTTP/2, TLS, or complex protocols
   - Even Cilium uses Envoy for L7 (not eBPF)

2. **"Crafting Interpreters"** (Bob Nystrom) - WASM runtime concepts

3. **Cloudflare Pingora**: https://github.com/cloudflare/pingora
   - Validates Rust for L7 proxies

4. **Gateway API**: https://gateway-api.sigs.k8s.io/

5. **wasmtime Book**: https://docs.wasmtime.dev/

### Code References

- **Cloudflare Pingora**: Rust proxy (validates architecture)
- **Kong**: Lua plugins (what we're improving upon)
- **Envoy**: C++ filters (shows need for safer extension model)

### Research Papers

- **ACM 2024**: "QUIC is not Quick Enough" (HTTP/3 is 45% slower)
- **Google**: "Maglev: A Fast and Reliable Software Network Load Balancer"
- **Cloudflare**: Pingora architecture (Rust proxy validates our approach)

### Internal Research

- **docs/research/IO_URING_ANALYSIS.md**: Why io_uring doesn't help L7 HTTP proxies (2024)

---

## REALISTIC SCOPE

### What RAUTA IS

**Production-ready Gateway API controller**
- Full Gateway API v1 support (GatewayClass, Gateway, HTTPRoute)
- Maglev load balancing
- Prometheus observability
- Validated in kind cluster

**Safe plugin extensibility**
- WASM runtime (wasmtime)
- Multi-language SDK (Rust, Go, TypeScript)
- Sandboxed execution (cannot crash gateway)
- Hot-reload support

**Learning project for Rust + WASM**
- Explore Rust async patterns (tokio, hyper)
- Learn WASM runtime integration
- Build production-quality systems

### What RAUTA IS NOT

**Not an XDP HTTP router**
- XDP sees raw packets (no TCP reassembly)
- Modern protocols (HTTP/2, TLS) require userspace
- Learning eBPF (Liz Rice) confirms: even Cilium uses Envoy for L7

**Not trying to beat Katran**
- Katran does L4 (10M pps)
- RAUTA does L7 (100K+ rps)
- Different problem spaces

**Not replacing Envoy/NGINX**
- Those are mature, battle-tested
- RAUTA is a focused tool with unique strengths (WASM plugins)

**Not building eBPF observability**
- Observability is out of scope
- Focus on Gateway API routing and WASM plugins
- Keep concerns separated

---

## DEVELOPMENT WORKFLOW

### Initial Setup

After cloning the repository:

```bash
# Install pre-commit hooks (REQUIRED)
./scripts/git-hooks/install.sh
```

**What the pre-commit hook does:**
- ✅ Blocks `.unwrap()` in production code
- ✅ Blocks `.expect()` in production code
- ✅ Blocks `panic!()` in production code
- ❌ Allows all of the above in test code (`#[cfg(test)]` modules)

**Why this matters:**
- Lock poisoning from `.unwrap()` on RwLock/Mutex causes cascading panics → gateway crash
- Metric registration failures shouldn't kill the gateway
- Production code must be resilient to failures

### Safe Lock Helpers (MANDATORY)

**NEVER use `.unwrap()` on locks.** Always use these helpers:

```rust
use crate::proxy::router::{safe_read, safe_write, safe_lock};

// ❌ BAD - Will panic on lock poisoning, cascades to all threads
let routes = self.routes.read().unwrap();

// ✅ GOOD - Recovers from poisoning, logs warning, continues
let routes = safe_read(&self.routes);

// ❌ BAD - RwLock write
let mut routes = self.routes.write().unwrap();

// ✅ GOOD
let mut routes = safe_write(&self.routes);

// ❌ BAD - Mutex
let inner = self.inner.lock().unwrap();

// ✅ GOOD
let inner = safe_lock(&self.inner);
```

**How the helpers work:**
```rust
/// Safe RwLock read with automatic poison recovery
#[inline]
fn safe_read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| {
        warn!("RwLock poisoned during read, recovering (data is still valid)");
        poisoned.into_inner()  // Extract data, continue operation
    })
}
```

**Why poison recovery is safe:**
- Lock poisoning means a thread panicked while holding the lock
- The DATA is still valid (Rust prevents memory corruption)
- Extracting the data and continuing is safer than cascading panics
- Warning is logged for observability

### Error Handling Rules

1. **Production Code**:
   - Use `Result<T, E>` and propagate errors with `?`
   - Use `Option<T>` with `.ok_or()` or early returns
   - Never use `.unwrap()` or `.expect()`
   - Graceful degradation (e.g., metrics failures shouldn't crash gateway)

2. **Test Code**:
   - `.unwrap()` and `.expect()` are FINE in tests
   - Tests should panic on unexpected failures
   - Use descriptive messages: `.expect("Should parse test certificate")`

3. **Metrics and Observability**:
   - Metric registration failures log warnings but don't crash
   - Gateway runs even with degraded observability
   - Better to serve traffic without metrics than not serve at all

### Commit Workflow

```bash
# Make changes
git add .

# Pre-commit hook runs automatically
git commit -m "feat: your change"

# If hook blocks you:
# 1. Check if unwrap/expect are in production code (fix them)
# 2. If they're in tests, the hook has a bug (override)
git commit --no-verify -m "feat: your change"
```

---

## DEFINITION OF DONE

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

## DEVELOPMENT ROADMAP

### Month 1-2: Production-Ready Gateway (Table Stakes)

**Week 1: EndpointSlice resolution**
- Dynamic backend discovery
- Pod IP updates without restart
- Health checking integration

**Week 2: HTTP/2 support**
- Multiplexing
- Header compression
- Server push (optional)

**Week 3: TLS termination**
- rustls integration
- SNI support
- Certificate rotation

**Week 4: Basic rate limiting and circuit breakers**
- Token bucket algorithm
- Per-route limits
- Failure detection

**Week 5-6: DaemonSet deployment model**
- Host network mode
- Node-local routing
- HA considerations

**Week 7-8: Testing and production hardening**
- Load testing (wrk, vegeta)
- Chaos testing (toxiproxy)
- Security review

### Month 3-4: WASM Plugin System (THE DIFFERENTIATOR)

**Week 9-10: WASM runtime integration**
- wasmtime setup
- Memory limits
- CPU limits
- Timeout handling

**Week 11-12: Plugin SDK and lifecycle hooks**
- Rust SDK
- Go SDK bindings
- TypeScript SDK (wit-bindgen)
- Request/response hooks

**Week 13-14: Built-in plugins**
- JWT authentication
- CORS handling
- Rate limiting (WASM-based)
- Request transformation

**Week 15-16: Plugin configuration and hot-reload**
- HTTPRoute annotations
- ConfigMap integration
- Reload without downtime
- Plugin versioning

### Month 5-6: Developer Experience and Launch

**Week 17-18: kubectl rauta plugin**
- Debugging tools
- Plugin logs
- Performance profiling

**Week 19-20: Plugin marketplace**
- Plugin registry
- Installation CLI
- Documentation site

**Week 21-22: Examples and tutorials**
- Getting started guide
- Plugin development tutorial
- Production deployment guide

**Week 23-24: Launch prep**
- Blog posts
- Conference talks
- Community building

---

## COMPETITIVE POSITIONING

**vs Kong**
- Kong: Lua plugins can crash gateway
- RAUTA: WASM plugins are sandboxed
- Expected: 10-15x faster (no LuaJIT overhead)

**vs Envoy**
- Envoy: C++ filters are dangerous to write
- RAUTA: WASM plugins in any language
- Benefit: Memory safe, easier to extend

**vs Traefik**
- Traefik: Limited plugin ecosystem
- RAUTA: Multi-language WASM support
- Benefit: Write plugins in Rust, Go, TypeScript

**vs NGINX**
- NGINX: C modules require recompilation
- RAUTA: Hot-reload WASM plugins
- Benefit: Zero-downtime updates

**Unique Differentiators:**
1. **Gateway API native** - Modern standard from day 1
2. **WASM plugins** - Safe, fast, multi-language extensibility
3. **Rust core** - Memory safety without garbage collection
4. **Maglev hashing** - Google's algorithm (proven at scale)
5. **Research-backed** - ACM papers, not hype

**The Pitch:**
> "RAUTA is the Gateway API controller you can safely extend with WASM plugins. Like Kong, but your plugins can't crash the gateway. Like Envoy, but you don't need to write C++. Write plugins in Rust, Go, or TypeScript - they run sandboxed with CPU and memory limits."

---

## FINAL MANIFESTO

**RAUTA is a production-focused learning project for building modern K8s infrastructure in Rust.**

**We're building:**
- Real-world Gateway API controller (working in kind cluster)
- Safe WASM plugin extensibility (the differentiator)
- Research-backed architecture (Cloudflare Pingora, wasmtime)
- Production-quality Rust (TDD, strong typing, pre-commit hooks)

**We're NOT building:**
- XDP HTTP parser (unrealistic for HTTP/2, TLS - Learning eBPF validates)
- Another Envoy clone (learn from Envoy, improve the plugin model)
- Experimental dead-end (every feature has production use case)
- eBPF observability (that's TAPIO's job - separation of concerns)

**Learn. Build. Ship.**

---

**Iron-clad routing with safe extensibility.**
