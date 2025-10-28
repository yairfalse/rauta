# ENVOY HTTP ROUTING & OBSERVABILITY ARCHITECTURE ANALYSIS

## 1. HTTP ROUTING ARCHITECTURE

### 1.1 Core Routing Hierarchy

Envoy uses a **hierarchical routing model**:

```
RouteConfiguration (top-level)
├── VirtualHosts (domain-based routing)
│   ├── Prefix/Path/Regex matching
│   ├── Header matching
│   ├── QueryParameter matching
│   └── RouteEntryImpl variants
│       ├── PrefixRouteEntryImpl
│       ├── PathRouteEntryImpl  (exact path match)
│       ├── RegexRouteEntryImpl (regex-based)
│       ├── ConnectRouteEntryImpl (CONNECT method)
│       ├── PathSeparatedPrefixRouteEntryImpl (trie-optimized)
│       └── UriTemplateMatcherRouteEntryImpl (RFC 6570)
└── Dynamic routing (xDS RDS)
```

**Key Files:**
- `/source/common/router/config_impl.h` - Route configuration (62KB)
- `/source/common/router/router.h` - Router filter interface (32KB)
- `/api/envoy/config/route/v3/route.proto` - Route configuration proto
- `/api/envoy/config/route/v3/route_components.proto` - Route components proto

### 1.2 Route Matching Strategy

**Two-level matching approach:**

1. **Virtual Host Selection** (fast path):
   - Exact domain match: `www.foo.com`
   - Suffix wildcard: `*.foo.com`, `*-bar.foo.com`
   - Prefix wildcard: `foo.*`, `foo-*`
   - Catch-all: `*`
   - Port-aware or port-agnostic matching

2. **Route Selection** (within VirtualHost):
   - **Matcher System**: Tree-based generic matcher (handles HTTP inputs)
   - **Path Matchers** (in order of specificity):
     - Exact path: `/api/users`
     - Prefix: `/api` (matches `/api/users`, `/api/posts`)
     - Regex: `^/v[0-9]+/.*` (compiled regex)
     - URI Template: `/items/{id}` (RFC 6570)
   - **Header Matchers**: `Host`, `Method`, custom headers
   - **Query Parameter Matchers**: Match on query params

### 1.3 Route Configuration Load Path

```cpp
// Key decision point in RouteCreator::createAndValidateRoute
switch (route_config.match().path_specifier_case()) {
case envoy::config::route::v3::RouteMatch::PathSpecifierCase::kPrefix:
  route = new PrefixRouteEntryImpl(...);  // Fast trie match
case kPath:
  route = new PathRouteEntryImpl(...);    // Exact string comparison
case kSafeRegex:
  route = new RegexRouteEntryImpl(...);   // Compiled PCRE
case kPathSeparatedPrefix:
  route = new PathSeparatedPrefixRouteEntryImpl(...);  // Optimized trie
case kPathMatchPolicy:
  route = new UriTemplateMatcherRouteEntryImpl(...);   // Template matching
}
```

### 1.4 Request Flow (Routing Decision)

```
1. Router::decodeHeaders()
2. route_entry = route_config.route(headers, stream_info, random_value)
   ├── VirtualHost::route(headers)  -- matches virtual host
   └── RouteEntryImplBase::route(headers)  -- matches specific route
3. Select upstream cluster
4. Route to backend
```

**Match Function Signature:**
```cpp
virtual RouteConstSharedPtr matches(
    const Http::RequestHeaderMap& headers,
    const StreamInfo::StreamInfo& stream_info,
    uint64_t random_value) const PURE;
```

### 1.5 TLS Termination

**Handled at listener level**, NOT in router:
- HTTP connection manager performs TLS termination
- Router only sees decrypted headers
- Uses scheme header (`:scheme`) to determine upstream TLS requirement
- `SslRedirectRoute` class handles HTTPS redirect (301) when TLS required

**Key insight:** TLS is NOT part of the routing decision in Envoy's design.

### 1.6 Router Performance Characteristics

**Optimization Strategies:**

1. **Route Entry Pooling**: Pre-allocated shared pointers
2. **Header Parser Optimization**: Group-level header parsing
   - Global headers (route config level)
   - VirtualHost headers
   - Route-specific headers
   - Specificity-sorted for efficient override
3. **Cached Matching Results**: Route decisions cached in StreamInfo
4. **Fast Trie Lookup**: PathSeparatedPrefixRouteEntryImpl uses trie for prefix matching

**Estimated Latency:**
- VirtualHost selection: O(log N) with suffix/prefix wildcard optimization
- Route selection: O(1) for exact/prefix, O(log N) for regex
- Total: <100μs per request (typical case)

---

## 2. CONFIGURATION MODEL

### 2.1 Static vs. Dynamic Configuration

**Static Routes** (compile-time):
```proto
route_config {
  name: "my_routes"
  virtual_hosts {
    name: "api_backend"
    domains: ["api.example.com"]
    routes {
      match { prefix: "/api/" }
      route { cluster: "backend_cluster" }
    }
  }
}
```

**Dynamic Routes (RDS/xDS):**
- Real-time route updates via Aggregated Discovery Service (ADS)
- Files: `/source/common/router/rds_impl.h`, `/source/router/scoped_rds.h`
- Updates applied per VirtualHost
- Virtual Host Discovery Service (VHDS) for dynamic virtual host addition

### 2.2 Configuration Provider Model

```cpp
// RouteConfigProvider: abstraction over static/dynamic routes
class RouteConfigProvider {
    virtual RouteConfigConstSharedPtr config() = 0;
    virtual absl::Status onConfigUpdate() = 0;
};

// Two implementations:
- StaticRouteConfigProvider  (static routes)
- RdsRouteConfigProvider     (dynamic RDS subscription)
```

**Update Mechanism:**
1. RdsRouteConfigSubscription watches xDS API
2. Receives new RouteConfiguration protos
3. Validates clusters exist
4. Applies update atomically (new route tree)
5. Triggers callbacks to listener filter chain

### 2.3 Hot Reload Strategy

**Zero-downtime route updates:**

1. **Atomic Swap**: New route tree replaces old atomically
2. **Versioning**: Each update has version_info
3. **Thread-local Storage**: Routes stored per-worker thread
4. **In-flight Requests**: Continue using old routes (no disruption)
5. **Callback Mechanism**: Update callbacks propagate to control plane

**Key Files:**
- `RdsRouteConfigSubscription`: Handles RDS API streaming
- `RouteConfigUpdateReceiverImpl`: Processes updates
- Thread-local store: Per-worker route cache

### 2.4 Route Configuration Validation

- **Cluster Validation**: All referenced clusters must exist (configurable)
- **Regex Validation**: PCRE regex compiled upfront (fail fast)
- **Retry Policy Validation**: Per-route retry policies validated
- **Rate Limiting Rules**: Rate limit matchers validated

---

## 3. OBSERVABILITY & METRICS

### 3.1 Router-Level Metrics

**Defined in `context_impl.h`:**

```c
#define ALL_ROUTER_STATS(COUNTER, GAUGE, HISTOGRAM, TEXT_READOUT, STATNAME)
  COUNTER(no_cluster)                    // No upstream cluster found
  COUNTER(no_route)                      // No matching route
  COUNTER(rq_direct_response)            // Direct response (no upstream)
  COUNTER(rq_redirect)                   // HTTP redirect (30x)
  COUNTER(rq_total)                      // Total requests processed
  COUNTER(rq_overload_local_reply)       // Overload manager triggered
  COUNTER(rq_reset_after_downstream_response_started)
  COUNTER(passthrough_internal_redirect_*)  // Internal redirect stats
  STATNAME(retry)                        // Retry grouping
```

**Metrics are**:
- Counters (monotonic increments)
- Gauges (instantaneous values)
- Histograms (latency distributions)
- Thread-safe per-worker recording

### 3.2 Access Logging

**Two-stage logging:**

1. **In-XDP/Early Logging**: Minimal buffering
2. **Async Logging**: Access logs written asynchronously

**Access Log Filters** (`access_log_impl.h`):
- `StatusCodeFilter`: Log on status code ranges
- `DurationFilter`: Log on request duration
- `AndFilter`/`OrFilter`: Composite logic
- `RuntimeFilter`: Feature-flag-based logging
- `NotHealthCheckFilter`: Exclude health checks

**Log Format**:
- Fully customizable format strings
- Access to all stream info (headers, status, timing, etc.)
- Buffered or immediate write modes

### 3.3 Histogram & Latency Tracking

**Implementation** (`histogram_impl.h`):

```cpp
class HistogramImpl : public Histogram {
public:
    Unit unit() const override { return unit_; }
    void recordValue(uint64_t value) override {
        parent_.deliverHistogramToSinks(*this, value);
    }
};
```

**Supported Histogram Buckets:**
- CircllHist library (efficient memory storage)
- Configurable bucket boundaries per stat
- Percentile computation (p50, p99, p99.9, etc.)

**Latency Metrics Tracked:**
- Request time (total)
- Upstream connect time
- First upstream byte time (TTFB)
- Per-try time
- Retry latency

### 3.4 Route-Specific Metrics

Each route can have its own stats:
```cpp
class RouteStatsContextImpl {
public:
    const RouteStats& stats() const override { return route_stats_; }
};

// RouteStats contains:
// - requests (counter)
// - active_requests (gauge)
// - total_match_count (counter)
```

### 3.5 Metrics Export/Scraping

**Stats Architecture** (`/source/common/stats/`):
- Symbol table (DeDup stat names)
- Hierarchical scope organization
- Thread-local statistics aggregation
- Pluggable stat sinks (Prometheus, statsd, etc.)

**Prometheus Exposure:**
- Via admin endpoint `/stats/prometheus`
- All metrics scraped in Prometheus format
- Cardinality-aware (prevent explosion)

---

## 4. LOAD BALANCING

### 4.1 Load Balancer Abstraction

```cpp
class LoadBalancer {
public:
    virtual HostSelectionResponse chooseHost(
        LoadBalancerContext& context) = 0;
};

// Available algorithms:
// - Random
// - Round Robin (RR)
// - Least Request
// - Ring Hash (Ketama)
// - Maglev Hash
// - Weighted Round Robin
```

### 4.2 Consistent Hash Algorithms

**Ring Hash** (`ring_hash_lb.h`):
```cpp
struct RingEntry {
    uint64_t hash_;
    HostConstSharedPtr host_;
};

std::vector<RingEntry> ring_;  // Sorted ring of (hash, host) pairs
```

- Ketama algorithm (used by Memcached)
- Min/max ring size configuration
- Hash function selectable (MurmurHash2, xxHash64)
- Supports weighted hosts

**Maglev Hash** (`maglev_lb.h`):
```cpp
// Table size: typically 65537 (prime)
class MaglevTable {
    std::vector<HostConstSharedPtr> table_;  // Original implementation
    // OR
    BitArray table_;  // Compact implementation
    std::vector<HostConstSharedPtr> host_table_;
};
```

- Google's Maglev algorithm (more uniform distribution than ring hash)
- Fixed table size (65537 recommended)
- Better host distribution (fewer hash collisions)
- Lower memory overhead with compact variant

**Hash Policy Input:**
```proto
message RouteAction {
  HashPolicy hash_policy {
    header { header_name: "user-id" }  // Hash on user-id header
    // OR
    cookie { name: "session-id" }      // Hash on cookie
    // OR
    connection_properties { ... }      // Hash on source IP + port
  }
}
```

### 4.3 Health Checking

- Active health checks (periodic HTTP requests)
- Passive health checking (circuit breaker on 5xx)
- Outlier detection (remove misbehaving hosts)
- Updates to load balancer ring/table when hosts change

### 4.4 Connection Pooling

**Per-Upstream Connection Pools:**
- One pool per (host, protocol, TLS config) tuple
- Configurable max connections per host
- Configurable max pending requests
- HTTP/1.1, HTTP/2, HTTP/3 support

---

## 5. PERFORMANCE PATTERNS

### 5.1 Zero-Copy Design

**Principles:**
- Routes are immutable after creation
- Headers passed by reference
- No deep copies of request/response data
- String views (absl::string_view) for header values

### 5.2 Per-CPU/Thread-Local Caching

```cpp
// Routes cached per-worker thread (thread-local storage)
class ThreadLocalRouteTable {
    std::shared_ptr<const RouteConfig> route_config_;
};
```

**Benefits:**
- Avoid contention on route lookups
- Each worker thread has own copy
- Updates broadcast to all workers atomically

### 5.3 Memory Management

**Object Pooling:**
- Upstream requests pre-allocated
- Header parser instances reused
- Buffer pre-allocation for common sizes

**Bounded Memory:**
- Route tables bounded (configurable max entries)
- Connection pools bounded per cluster
- No unbounded allocations in hot path

### 5.4 Hot Path Optimization

**Fast Paths:**
1. Cache hit for same VirtualHost (common in keep-alive)
2. Exact path match (O(1) hash lookup)
3. Prefix match via optimized trie
4. Ring hash lookup (single array access + linear probe)

**Slow Paths (still bounded):**
- Regex matching (compiled upfront)
- Header matching (loop through headers)
- Route creation (happens once at boot)

### 5.5 Latency Budget Allocation

**Estimated breakdown (per request):**
- VirtualHost selection: 1-5 μs
- Route selection: 1-10 μs
- Load balancer selection: 1-5 μs (ring hash) or 0.1 μs (maglev)
- Upstream pool lookup: 1-2 μs
- **Total: ~5-25 μs** (excluding network)

---

## 6. KEY ARCHITECTURAL INSIGHTS FOR RAUTA

### 6.1 What to Adopt

1. **Hierarchical Routing Model**
   - VirtualHost (domain) → Route (path/method) hierarchy is proven
   - Separate concerns (host routing vs. path routing)

2. **Multiple Path Match Types**
   - Exact, prefix, regex, URI template matching all have use cases
   - Don't force one strategy; support all

3. **Route Versioning & Atomic Updates**
   - Use version numbers for routes
   - Atomic swap (old tree → new tree)
   - No in-flight request disruption

4. **Per-Route Stats**
   - Critical for debugging traffic patterns
   - Route name → metrics correlation

5. **Thread-Local Route Caching**
   - Eliminates locking in hot path
   - Updates broadcast to all workers

6. **Header Parser Abstraction**
   - Route can specify header mutations
   - Header parser instance per (global, vhost, route) triplet
   - Specificity-based override rules

7. **Consistent Hashing for Affinity**
   - Use Maglev over Ring Hash (better distribution)
   - Hash policy customizable (headers, cookies, IP)

8. **Metrics Design**
   - Per-route counters
   - Request latency histograms
   - Active request gauges
   - Filter-by-status-code capability

### 6.2 What to Avoid

1. **Don't Route at L7 in XDP** ❌
   - Envoy routes at L7 but in **userspace**
   - HTTP parsing in XDP is fragile (multi-packet issues)
   - Instead: Parse & identify flow in XDP, route decisions in userspace

2. **Don't Unbounded Allocations**
   - Envoy uses fixed-size tables, pre-allocated pools
   - RAUTA must use bounded data structures

3. **Don't Ignore Per-Filter Config**
   - Envoy allows per-route filter configuration
   - Very powerful for feature flagging, circuit breaking per route

4. **Don't Skip Hot Reload Testing**
   - Atomic route updates are complex
   - Test thoroughly (in-flight requests during update)

5. **Don't Mix Synchronous/Async Stats**
   - Pick async (Envoy style) for high concurrency
   - Avoid lock contention

### 6.3 RAUTA-Specific Recommendations

**Stage 1 (Rust Ingress Controller):**

```rust
// Routing model for RAUTA
pub struct RautaRoute {
    pub name: String,
    pub matcher: PathMatcher,  // Exact/Prefix/Regex
    pub method: HttpMethod,
    pub backends: Vec<Backend>,
    pub timeout: Duration,
    pub retry_policy: RetryPolicy,
    pub stats: Arc<RouteStats>,
}

pub enum PathMatcher {
    Exact(String),
    Prefix(String),
    Regex(CompiledRegex),
}

pub struct VirtualHost {
    pub name: String,
    pub domains: Vec<Domain>,
    pub routes: Vec<RautaRoute>,  // Matched in order
}

pub struct RouteConfig {
    pub version: u64,  // For versioning
    pub virtual_hosts: Vec<VirtualHost>,
}
```

**Route Selection Algorithm:**

```rust
impl RouteConfig {
    pub fn select_route(
        &self,
        req: &HttpRequest,
        random: &mut Rng,
    ) -> Option<&RautaRoute> {
        // 1. Select VirtualHost by domain (fast trie)
        let vhost = self.select_vhost(&req.host)?;
        
        // 2. Select Route within VirtualHost (in order)
        for route in &vhost.routes {
            if route.matcher.matches(&req.path) &&
               route.method.matches(req.method) {
                return Some(route);
            }
        }
        None
    }
}
```

**Observability:**

```rust
pub struct RouteStats {
    pub requests_total: Counter,
    pub requests_active: Gauge,
    pub request_latency_us: Histogram,  // with p50, p99, p99.9
    pub upstream_latency_us: Histogram,
    pub errors_by_type: CounterMap<ErrorType>,
}

pub struct RautaMetrics {
    pub routes: HashMap<String, Arc<RouteStats>>,
    pub vhosts: HashMap<String, Arc<VhostStats>>,
}
```

**Load Balancing:**

```rust
// Use Maglev for affinity
pub struct MaglevLB {
    table: Vec<HostIdx>,  // Size: 65537
    hash_policy: HashPolicy,  // Source IP or custom
}

impl MaglevLB {
    pub fn select_host(&self, req_hash: u64) -> HostIdx {
        self.table[req_hash % self.table.len()]
    }
}
```

---

## 7. IMPLEMENTATION REFERENCES

### Key Files to Study

| File | Lines | Purpose |
|------|-------|---------|
| `/source/common/router/config_impl.h` | 1200+ | Route config + virtual host impl |
| `/source/common/router/router.h` | 900+ | Router filter (main routing logic) |
| `/source/common/router/router.cc` | 3000+ | Router implementation |
| `/source/extensions/load_balancing_policies/maglev/maglev_lb.h` | 200+ | Maglev algorithm |
| `/source/extensions/load_balancing_policies/ring_hash/ring_hash_lb.h` | 150+ | Ring hash algorithm |
| `/source/common/stats/histogram_impl.h` | 150+ | Histogram stats |
| `/source/common/access_log/access_log_impl.h` | 200+ | Access logging |
| `/api/envoy/config/route/v3/route.proto` | 200+ | Route config proto |
| `/api/envoy/config/route/v3/route_components.proto` | 800+ | Route components proto |

### Learn-From Patterns

1. **MatchTree<DataType>** - Generic matcher (Envoy's new approach)
   - Replaces old hard-coded route matching
   - Type-safe matcher factories
   - Recursive tree building

2. **HeaderParser** - Header mutation system
   - Pre-compiled header parsers
   - Formatter context for dynamic values
   - Lazy evaluation of header mutations

3. **RouteEntry Polymorphism** - Multiple route match types
   - PrefixRouteEntryImpl (trie-optimized)
   - RegexRouteEntryImpl (pre-compiled regex)
   - PathRouteEntryImpl (exact match)
   - Factory pattern to create right variant

4. **Scoped RDS** - Per-scope route configs
   - Different route configs per listener/scope
   - Inheritance of routes from parent scope

---

## CONCLUSION

**Envoy's routing architecture is:**
- ✅ Hierarchical (VirtualHost → Route)
- ✅ Extensible (pluggable matcher types)
- ✅ Observable (per-route metrics)
- ✅ Performant (<25μs per request)
- ✅ Reliable (hot reload without data loss)

**For RAUTA Stage 1:**
- Adopt Envoy's hierarchical model
- Implement route versioning + atomic updates
- Track per-route latency + error rates
- Use Maglev hashing for backend selection
- Design for bounded memory (no unbounded maps)
