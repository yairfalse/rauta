# RAUTA: eBPF L7 Ingress Controller - Rust + Aya Guidelines

**RAUTA = Iron-clad routing at wire speed** ⚡🦀

## ⚠️ CRITICAL: Project Nature

**THIS IS AN EXPERIMENTAL LEARNING PROJECT**
- **Goal**: Explore pushing L7 HTTP routing into XDP (kernel space)
- **Language**: 100% Rust (userspace) + eBPF C/Rust (kernel)
- **Status**: 🚧 EXPERIMENTAL - Learning in public
- **Performance Target**: L7 routing at L4 speeds (<10μs per packet)

## 🎯 PROJECT MISSION

**Challenge**: Can we do L7 HTTP routing in XDP instead of userspace?

**Inspiration**:
- **Katran** (Facebook): L4 load balancing in XDP (10M pps, sub-microsecond)
- **Cilium**: Full K8s CNI with eBPF dataplane (L3-L7 policies)
- **Envoy**: L7 proxy in userspace (rich features, slower)
- **RAUTA**: L7 HTTP routing in XDP (crazy fast, limited features)

**The Gap**: Everyone does L4 in XDP or L7 in userspace. Nobody does L7 **routing** in XDP.

## 🏗️ ARCHITECTURE PHILOSOPHY

### Brendan Gregg Principle: eBPF Captures, Userspace Parses

```
┌─────────────────────────────────────────────┐
│   XDP Program (rauta.bpf.c / rauta.bpf.rs) │
│   - Parse ethernet → IP → TCP → HTTP       │
│   - Extract: method, path, host header     │
│   - Match routing rules (CH ring lookup)   │
│   - Forward via XDP_TX or encapsulate      │
└──────────────────┬──────────────────────────┘
                   │ Ring Buffer (metrics/logs)
                   ▼
┌─────────────────────────────────────────────┐
│   Rust Control Plane (Aya framework)       │
│   - Configure routing rules                 │
│   - Update BPF maps (routes, backends)     │
│   - Collect metrics via ring buffer        │
│   - Kubernetes integration (watch Ingress) │
└─────────────────────────────────────────────┘
```

**Why This Works**:
- XDP sees packets **before** TCP stack allocation (zero-copy)
- HTTP routing is stateless (method + path + host → backend)
- Consistent hashing ensures affinity without connection tracking
- Metrics/logs handled in userspace (eBPF just increments counters)

## 🔥 PERFORMANCE REQUIREMENTS

### Target: Katran-Class Performance for L7

- **Packet Processing**: <10μs per packet (including HTTP parsing!)
- **Throughput**: 1M+ requests/second on commodity hardware
- **Latency**: p99 < 100μs (vs Envoy p99 ~5ms)
- **Memory**: Constant memory usage (BPF maps are bounded)
- **Zero Allocations**: XDP path must not allocate memory

### Performance Patterns from Katran

```rust
// ✅ Per-CPU BPF maps (lock-free access)
#[map]
static ROUTING_TABLE: PerCpuHashMap<RouteKey, BackendId> =
    PerCpuHashMap::with_max_entries(65536, 0);

// ✅ Consistent hashing (Maglev algorithm)
fn select_backend(route_key: &RouteKey, backends: &[Backend]) -> BackendId {
    let hash = maglev_hash(route_key);
    backends[hash % backends.len()].id
}

// ✅ RSS-friendly source IP for IPIP encapsulation
fn generate_encap_src_ip(client_ip: u32, client_port: u16) -> u32 {
    // Mix client IP + port for consistent RSS steering
    (client_ip ^ (client_port as u32)) | VIP_PREFIX
}
```

## 🦀 RUST + AYA REQUIREMENTS

### Language Requirements
- **THIS IS A RUST PROJECT** - All userspace code in Rust
- **eBPF Code**: C (stable) or Aya-RS (experimental Rust eBPF)
- **NO GO CODE** - Unlike Katran's C++ + Go, we use Rust everywhere
- **STRONG TYPING ONLY** - No `Box<dyn Any>` or runtime type checking

### Aya Framework Patterns

```rust
// ✅ Load XDP program with Aya
use aya::{Bpf, programs::Xdp};

let mut bpf = Bpf::load(include_bytes_aligned!(
    "../../target/bpfel-unknown-none/release/rauta"
))?;

let program: &mut Xdp = bpf.program_mut("rauta_ingress").unwrap().try_into()?;
program.load()?;
program.attach("eth0", XdpFlags::SKB_MODE)?;

// ✅ Access BPF maps from Rust
use aya::maps::HashMap;

let mut routes: HashMap<_, RouteKey, BackendList> =
    HashMap::try_from(bpf.map_mut("ROUTES")?)?;

routes.insert(
    RouteKey { method: GET, path_hash: hash("/api") },
    BackendList { backends: [...], count: 3 },
    0, // flags
)?;
```
**NO STUBS. NO TODOs. COMPLETE CODE ONLY.**

### 4. TDD Workflow (RED → GREEN → REFACTOR)

**MANDATORY**: All code must follow strict Test-Driven Development

#### RED Phase: Write Failing Tests First
```go
// Step 1: Write test that FAILS (RED)
func TestLinkProcessor_SYNTimeout(t *testing.T) {
    proc := NewLinkProcessor()  // ❌ Undefined - test fails
    require.NotNil(t, proc)

    evt := NetworkEventBPF{
        OldState: TCP_SYN_SENT,
        NewState: TCP_CLOSE,
        SrcIP:    0x0100007f,
        DstIP:    0x6401a8c0,
    }

    domainEvt := proc.Process(context.Background(), evt)
    require.NotNil(t, domainEvt)
    assert.Equal(t, "link_failure", domainEvt.Subtype)
}

// Step 2: Verify test compilation FAILS
// $ go test ./...
// # undefined: NewLinkProcessor ✅ RED phase confirmed
```

#### GREEN Phase: Minimal Implementation
```go
// Step 3: Write MINIMAL code to pass test
type LinkProcessor struct {}

func NewLinkProcessor() *LinkProcessor {
    return &LinkProcessor{}
}

func (p *LinkProcessor) Process(ctx context.Context, evt NetworkEventBPF) *domain.ObserverEvent {
    if evt.OldState == TCP_SYN_SENT && evt.NewState == TCP_CLOSE {
        return &domain.ObserverEvent{
            Type:    "network",
            Subtype: "link_failure",
            NetworkData: &domain.NetworkEventData{
                SrcIP: convertIPv4(evt.SrcIP),
                DstIP: convertIPv4(evt.DstIP),
            },
        }
    }
    return nil
}

// Step 4: Verify tests PASS
// $ go test ./...
// PASS ✅ GREEN phase confirmed
```

#### REFACTOR Phase: Improve Code Quality
```go
// Step 5: Add edge cases (IPv6, validation, etc.)
func TestLinkProcessor_SYNTimeout_IPv6(t *testing.T) {
    // Test IPv6 handling
}

// Step 6: Refactor for better design
func (p *LinkProcessor) Process(ctx context.Context, evt NetworkEventBPF) *domain.ObserverEvent {
    if !p.isSYNTimeout(evt) {
        return nil
    }
    return p.createLinkFailureEvent(evt, "syn_timeout")
}

// Step 7: Verify tests still PASS after refactor
// $ go test ./...
// PASS ✅ REFACTOR complete
```

#### TDD Checklist
- [ ] **RED**: Write failing test first
- [ ] **RED**: Verify compilation fails or test fails
- [ ] **GREEN**: Write minimal implementation
- [ ] **GREEN**: Verify all tests pass
- [ ] **REFACTOR**: Add edge cases, improve design
- [ ] **REFACTOR**: Verify tests still pass
- [ ] **Commit**: `git add . && git commit -m "feat: ..."` (< 30 lines)

**Example Session** (Network Observer Processors):
```bash
# LinkProcessor (TDD - 3 commits)
1. RED:   Write TestLinkProcessor_SYNTimeout → FAIL ✅
2. GREEN: Implement processor_link.go → PASS ✅
3. COMMIT: git commit -m "feat: add LinkProcessor (TDD)"

# Add IPv6 support (TDD - 2 commits)
1. RED:   Write TestLinkProcessor_SYNTimeout_IPv6 → FAIL ✅
2. GREEN: Add IPv6 handling to createLinkFailureEvent → PASS ✅
3. COMMIT: git commit -m "fix: handle IPv6 in LinkProcessor"
```

### 5. eBPF Development Pattern (Brendan Gregg Approach)

**MANDATORY**: Follow single eBPF program + Go processor pattern

#### Architecture: eBPF Captures, Userspace Parses

```
┌─────────────────────────────────────────────────┐
│   eBPF Kernel (network_monitor.c)              │
│   - Single eBPF program (NO new programs!)     │
│   - Captures: TCP states, UDP traffic, IPs     │
│   - Minimal processing (just capture data)     │
└──────────────────┬──────────────────────────────┘
                   │ Ring Buffer
                   ▼
┌─────────────────────────────────────────────────┐
│   Go Userspace (processEventsStage)            │
│   Processor Chain:                              │
│   1. LinkProcessor   → link_failure             │
│   2. DNSProcessor    → dns_query, dns_response  │
│   3. StatusProcessor → http_connection          │
│   4. Fallback        → legacy events            │
└──────────────────┬──────────────────────────────┘
                   │
                   ▼
         domain.NetworkEventData
         (Type + Subtype pattern)
```

#### Why This Pattern Works

**Brendan Gregg BPF Performance Tools (Chapter 10)**:
> "eBPF should capture, userspace should parse. Parsing complex protocols in eBPF is slow and error-prone. Let eBPF collect the raw data, then parse it in userspace where you have full language features."

**Performance**:
- eBPF parsing: ~500ns per packet (slow, limited instructions)
- Go parsing: ~50ns per packet (10x faster!)
- Ring buffer already copies to userspace - parsing there is free

**Benefits**:
1. **Single eBPF program** - Lower kernel overhead, simpler lifecycle
2. **Flexible parsing** - Go is easier to debug than eBPF C code
3. **No BTF dependencies** - Don't need kernel struct definitions for DNS/HTTP parsing
4. **Easier testing** - Can unit test processors without eBPF
5. **IPv4 + IPv6 support** - Handle both address families in Go

#### Implementation Pattern

**Step 1: Design Processor** (following TDD RED phase)
```go
// processor_dns.go - RED phase (write test first!)
func TestDNSProcessor_DetectQuery(t *testing.T) {
    proc := NewDNSProcessor()  // Will fail - doesn't exist yet
    evt := NetworkEventBPF{
        Protocol: IPPROTO_UDP,
        DstPort:  53,  // DNS port
    }

    domainEvt := proc.Process(context.Background(), evt)
    require.NotNil(t, domainEvt)
    assert.Equal(t, "dns_query", domainEvt.Subtype)
}
```

**Step 2: Implement Processor** (GREEN phase)
```go
// processor_dns.go
type DNSProcessor struct {
    // Future: OTEL metrics
}

func NewDNSProcessor() *DNSProcessor {
    return &DNSProcessor{}
}

func (p *DNSProcessor) Process(ctx context.Context, evt NetworkEventBPF) *domain.ObserverEvent {
    // Only process UDP traffic
    if evt.Protocol != IPPROTO_UDP {
        return nil
    }

    // Check if DNS port (53)
    if evt.DstPort != 53 && evt.SrcPort != 53 {
        return nil
    }

    // Handle IPv4 AND IPv6 (MANDATORY!)
    var srcIP, dstIP string
    if evt.Family == AF_INET {
        srcIP = convertIPv4(evt.SrcIP)
        dstIP = convertIPv4(evt.DstIP)
    } else {
        srcIP = convertIPv6(evt.SrcIPv6)
        dstIP = convertIPv6(evt.DstIPv6)
    }

    // Use EXISTING domain model (no new structs!)
    return &domain.ObserverEvent{
        Type:    string(domain.EventTypeNetwork),
        Subtype: "dns_query",
        NetworkData: &domain.NetworkEventData{
            Protocol: "DNS",
            SrcIP:    srcIP,
            DstIP:    dstIP,
            SrcPort:  evt.SrcPort,
            DstPort:  evt.DstPort,
        },
    }
}
```


## 📋 HTTP PARSING IN XDP

### Challenge: Parsing L7 in Kernel Space

**Why It's Hard**:
- XDP sees raw packets (no TCP reassembly)
- Limited verifier instructions (1M instructions max)
- No loops (must unroll everything)
- Can't access kernel TCP structs easily

**Strategy**: Parse HTTP in Single Packet

```c
// eBPF C code (rauta.bpf.c)
SEC("xdp")
int rauta_ingress(struct xdp_md *ctx) {
    void *data = (void *)(long)ctx->data;
    void *data_end = (void *)(long)ctx->data_end;

    // Parse ethernet → IP → TCP
    struct ethhdr *eth = data;
    if ((void *)(eth + 1) > data_end) return XDP_PASS;

    struct iphdr *ip = (void *)(eth + 1);
    if ((void *)(ip + 1) > data_end) return XDP_PASS;

    struct tcphdr *tcp = (void *)ip + (ip->ihl * 4);
    if ((void *)(tcp + 1) > data_end) return XDP_PASS;

    // HTTP parsing (only for first packet with HTTP request)
    void *http_start = (void *)tcp + (tcp->doff * 4);
    if ((void *)http_start + 16 > data_end) return XDP_PASS;

    // Check for HTTP methods (GET, POST, PUT, DELETE)
    struct http_request req = {0};
    if (parse_http_method(http_start, data_end, &req) < 0) {
        return XDP_PASS;  // Not HTTP or fragmented
    }

    // Extract path (up to 256 bytes)
    if (parse_http_path(http_start, data_end, &req) < 0) {
        return XDP_PASS;
    }

    // Lookup routing rule
    struct route_key key = {
        .method = req.method,
        .path_hash = hash_path(req.path, req.path_len),
    };

    struct backend *be = bpf_map_lookup_elem(&ROUTING_TABLE, &key);
    if (!be) {
        return XDP_PASS;  // No route, let kernel handle
    }

    // Forward to backend (XDP_TX hairpin or IPIP encapsulation)
    return forward_to_backend(ctx, be);
}
```

### Limitations We Accept

1. **Single-packet HTTP only**: Multi-packet requests fall back to userspace
2. **Limited path length**: Max 256 bytes (99% of requests)
3. **No HTTP/2 or HTTP/3**: Only HTTP/1.1 text-based protocol
4. **No TLS termination in XDP**: TLS handled in userspace (Envoy, Nginx)

**This is OK!** - We're optimizing the 80% case (simple HTTP/1.1 routing).


```

```

## 🧪 TESTING STRATEGY

### TDD for Control Plane (Rust)

```rust
// ✅ Test route configuration
#[test]
fn test_add_route() {
    let mut ctrl = ControlPlane::new().unwrap();

    ctrl.add_route(Route {
        method: HttpMethod::GET,
        path: "/api/users".into(),
        backends: vec![
            Backend { ip: "10.0.1.1".parse().unwrap(), port: 8080 },
            Backend { ip: "10.0.1.2".parse().unwrap(), port: 8080 },
        ],
    }).unwrap();

    // Verify BPF map updated
    let routes = ctrl.get_routes().unwrap();
    assert_eq!(routes.len(), 1);
}

// ✅ Test consistent hashing
#[test]
fn test_maglev_distribution() {
    let backends = vec![
        Backend { ip: "10.0.1.1".parse().unwrap(), port: 8080 },
        Backend { ip: "10.0.1.2".parse().unwrap(), port: 8080 },
        Backend { ip: "10.0.1.3".parse().unwrap(), port: 8080 },
    ];

    // Simulate 10k requests
    let mut distribution = HashMap::new();
    for i in 0..10_000 {
        let client_ip = Ipv4Addr::new(192, 168, 1, (i % 255) as u8);
        let backend = select_backend_maglev(&client_ip, 12345, &backends);
        *distribution.entry(backend).or_insert(0) += 1;
    }

    // Each backend should get ~33% (within 5% variance)
    for count in distribution.values() {
        let percentage = (*count as f64) / 10_000.0;
        assert!((percentage - 0.33).abs() < 0.05);
    }
}
```

### BPF Unit Testing (BPF selftests framework)

```c
// test_http_parser.c
#include "http_parser.h"

// Test GET request parsing
void test_parse_get_request() {
    const char *packet = "GET /api/users HTTP/1.1\r\nHost: example.com\r\n\r\n";
    struct http_request req = {0};

    int ret = parse_http_method(packet, packet + strlen(packet), &req);
    assert(ret == 0);
    assert(req.method == HTTP_GET);

    ret = parse_http_path(packet, packet + strlen(packet), &req);
    assert(ret == 0);
    assert(memcmp(req.path, "/api/users", 10) == 0);
}
```

### Integration Testing (Real Traffic)

```bash
# Generate HTTP traffic with wrk
wrk -t12 -c400 -d30s http://rauta-lb/api/test

# Verify XDP processing
sudo bpftool prog show
sudo bpftool map dump name ROUTING_TABLE

# Check metrics
rautactl metrics
# Output:
# packets_processed: 1,234,567
# packets_routed: 1,200,000
# packets_dropped: 0
# avg_latency_us: 8.2
# p99_latency_us: 42
```

## 📊 PERFORMANCE BENCHMARKING

### Mandatory Benchmarks

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_maglev_lookup(c: &mut Criterion) {
    let backends = generate_backends(100);
    let maglev_table = build_maglev_table(&backends, 65537);

    c.bench_function("maglev_lookup", |b| {
        b.iter(|| {
            let hash = black_box(12345u64);
            let backend = maglev_table[(hash % 65537) as usize];
            black_box(backend)
        })
    });
}

criterion_group!(benches, bench_maglev_lookup);
criterion_main!(benches);
```

### Performance Targets

- **Maglev lookup**: <10ns (pure memory lookup)
- **Route configuration**: <100μs (update BPF map)
- **Control plane memory**: <50MB resident
- **XDP packet processing**: <10μs (measured with bpftrace)

## 🔒 SAFETY REQUIREMENTS

### Rust Safety

```rust
// ✅ Safe BPF map access
let backend = routes.get(&key, 0)
    .map_err(|e| Error::MapLookup(e))?
    .ok_or(Error::RouteNotFound)?;

// ❌ NEVER use unwrap() in hot paths
let backend = routes.get(&key, 0).unwrap();  // PANIC in production!

// ✅ Bounded allocations only
let mut path_buffer = ArrayString::<256>::new();
path_buffer.push_str(&request.path)?;

// ❌ NEVER unbounded allocation from packet data
let path = String::from_utf8(packet_data.to_vec()).unwrap();
```

### BPF Verifier Compliance

```c
// ✅ Always bounds check before packet access
if ((void *)(eth + 1) > data_end) return XDP_PASS;

// ✅ Verifier-friendly loops (unrolled)
#pragma unroll
for (int i = 0; i < MAX_PATH_LEN; i++) {
    if (http_start + i >= data_end) break;
    path[i] = http_start[i];
    if (path[i] == ' ') break;
}

// ❌ NEVER dynamic loops (verifier rejects)
while (*ptr != ' ') {  // BPF verifier error!
    ptr++;
}
```

## 🎯 KUBERNETES INTEGRATION

### Watch Ingress Resources

```rust
use kube::{Api, Client, runtime::watcher};
use k8s_openapi::api::networking::v1::Ingress;

async fn watch_ingress() -> Result<()> {
    let client = Client::try_default().await?;
    let ingresses: Api<Ingress> = Api::all(client);

    let watcher = watcher(ingresses, watcher::Config::default());

    tokio::pin!(watcher);
    while let Some(event) = watcher.try_next().await? {
        match event {
            watcher::Event::Applied(ing) => {
                update_routes_from_ingress(&ing)?;
            }
            watcher::Event::Deleted(ing) => {
                remove_routes_from_ingress(&ing)?;
            }
            _ => {}
        }
    }

    Ok(())
}
```

### Sync Ingress → BPF Routes

```rust
fn update_routes_from_ingress(ing: &Ingress) -> Result<()> {
    let rules = ing.spec.as_ref()
        .and_then(|s| s.rules.as_ref())
        .ok_or(Error::NoRules)?;

    for rule in rules {
        let host = rule.host.as_deref().unwrap_or("*");
        let paths = rule.http.as_ref()
            .and_then(|h| h.paths.as_ref())
            .ok_or(Error::NoPaths)?;

        for path in paths {
            let route = Route {
                method: HttpMethod::ALL,  // Ingress doesn't specify method
                path: path.path.clone().unwrap_or_else(|| "/".into()),
                host: host.into(),
                backends: resolve_service_endpoints(&path.backend)?,
            };

            bpf_add_route(&route)?;
        }
    }

    Ok(())
}
```

## 🚀 DEPLOYMENT PATTERNS

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
      hostNetwork: true  # Access host network interfaces
      containers:
      - name: rauta
        image: rauta:v0.1.0
        securityContext:
          privileged: true  # Required for XDP
          capabilities:
            add: ["NET_ADMIN", "SYS_ADMIN", "BPF"]
        env:
        - name: RAUTA_INTERFACE
          value: "eth0"
        - name: RAUTA_XDP_MODE
          value: "native"  # or "skb" for testing
```

## 📚 LEARNING RESOURCES

### Must Read

1. **BPF Performance Tools** (Brendan Gregg) - Chapter 10: Networking
2. **Katran Paper**: https://engineering.fb.com/2018/05/22/open-source/open-sourcing-katran/
3. **Cilium Architecture**: https://docs.cilium.io/en/stable/concepts/ebpf/
4. **Aya Book**: https://aya-rs.dev/book/

### Code References

- **Katran**: https://github.com/facebookincubator/katran
- **Cilium**: https://github.com/cilium/cilium
- **Aya Examples**: https://github.com/aya-rs/aya/tree/main/aya/examples

## ⚠️ LIMITATIONS & FUTURE WORK

### Current Limitations

1. **HTTP/1.1 only** - No HTTP/2, HTTP/3, gRPC
2. **Single-packet requests** - Multi-packet requests fall back
3. **No TLS in XDP** - TLS termination in userspace
4. **Limited path matching** - Exact match or prefix only (no regex)
5. **IPv4 focus** - IPv6 support is future work

### Future Exploration

- [ ] HTTP/2 frame parsing in XDP (ambitious!)
- [ ] eBPF socket-level TLS offload (BPF_PROG_TYPE_SOCK_OPS)
- [ ] Integration with service mesh (mTLS between backends)
- [ ] Advanced routing (weighted, canary, A/B testing)
- [ ] Observability (OTEL traces from XDP?)

## 🎖️ DEFINITION OF DONE

A feature is complete when:

- [ ] Design documented in `docs/` (what problem, what solution)
- [ ] Rust tests passing (control plane logic)
- [ ] BPF tests passing (packet parsing, routing)
- [ ] Integration test with real HTTP traffic (wrk benchmark)
- [ ] Performance meets targets (<10μs packet processing)
- [ ] Code reviewed (preferably by eBPF expert!)
- [ ] Documentation updated (README, examples)

**NO STUBS. NO TODOs. COMPLETE CODE OR NOTHING.**

## 🏆 FINAL MANIFESTO

**RAUTA is an experiment to push the boundaries of what's possible with eBPF.**

We're not trying to replace Envoy or Nginx. We're exploring:
- Can L7 routing be fast enough in XDP?
- What's the performance ceiling for HTTP load balancing?
- How far can we push Rust + eBPF integration?

**Learn. Experiment. Share. Build fast. 🦀⚡**

---

**Finnish Tool Ecosystem**:
- **URPO**: Trace explorer 🔍
- **TAPIO**: K8s observer 🌲
- **AHTI**: Correlation engine 🌊
- **ELAVA**: AWS scanner 💚
- **RAUTA**: Ingress controller ⚙️ (iron)

**Iron-clad routing at wire speed.**

---

## 📊 OBSERVABILITY

**Prometheus Metrics**: See [docs/PROMETHEUS_METRICS.md](docs/PROMETHEUS_METRICS.md)

Key patterns from Cloudflare ebpf_exporter:
- BPF maps store numeric IDs → Rust decoders convert to labels
- OTEL dots → Prometheus underscores (`k8s.pod.name` → `k8s_pod_name`)
- Per-CPU aggregation before export
- Exponential buckets for latency histograms
- Non-blocking metric collection

**Observability Stack**:
```
XDP → BPF maps → Prometheus /metrics → Grafana
   └→ Ring buffer → OTLP traces → Jaeger/Tempo
```
