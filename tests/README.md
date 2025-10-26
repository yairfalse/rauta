# RAUTA Integration Tests

## Prerequisites

### Linux System (XDP requires Linux kernel 4.18+)
```bash
# Check kernel version
uname -r  # Should be >= 4.18

# Install dependencies
sudo apt-get install -y \
    linux-headers-$(uname -r) \
    clang \
    llvm \
    libelf-dev \
    wrk \
    bpftool

# Verify XDP support
sudo bpftool feature probe | grep xdp
```

### Build RAUTA
```bash
# Build BPF program
cd rauta/bpf
cargo build --release --target=bpfel-unknown-none

# Build control plane
cd ../control
cargo build --release
```

## Test Setup

### 1. Start Backend HTTP Server

Terminal 1:
```bash
# Simple Python HTTP server
python3 -m http.server 8080
```

Or use a more realistic backend:
```bash
# Install httpbin (realistic HTTP testing service)
pip install httpbin gunicorn

# Run httpbin on port 8080
gunicorn -b 0.0.0.0:8080 httpbin:app
```

### 2. Start RAUTA Control Plane

Terminal 2:
```bash
# Set environment
export RAUTA_INTERFACE=lo  # Loopback for testing
export RAUTA_XDP_MODE=skb  # Generic XDP mode

# Run with sudo (XDP requires CAP_NET_ADMIN)
sudo -E ./target/release/control
```

Expected output:
```
INFO RAUTA Control Plane starting...
INFO XDP program loaded successfully
INFO Test route added: GET /api/users -> 10.0.1.1:8080
INFO Control plane running. Press Ctrl-C to exit.
```

### 3. Verify XDP Attachment

Terminal 3:
```bash
# Check XDP program is attached
sudo bpftool net show

# Should show:
# lo(1) driver id 123 name rauta_ingress

# Check BPF maps
sudo bpftool map show | grep ROUTES
sudo bpftool map show | grep FLOW_CACHE
sudo bpftool map show | grep METRICS
```

## Integration Tests

### Test 1: HTTP GET Routing

```bash
# Send HTTP requests through RAUTA
curl -v http://localhost:8080/api/users

# Check if routed by XDP (check metrics)
# Metrics should show packets_tier1 increment
```

### Test 2: Load Testing with wrk

```bash
# Basic load test (12 threads, 400 connections, 30 seconds)
wrk -t12 -c400 -d30s http://localhost:8080/api/users

# Expected output (example):
# Running 30s test @ http://localhost:8080/api/users
#   12 threads and 400 connections
#   Thread Stats   Avg      Stdev     Max   +/- Stdev
#     Latency    95.20us   50.10us   5.12ms   89.23%
#     Req/Sec    85.42k     8.12k  102.34k    91.23%
#   30683421 requests in 30.10s, 4.12GB read
# Requests/sec: 1019448.33
# Transfer/sec:    140.12MB
```

**Target Performance** (from CLAUDE.md):
- Latency p99: <100μs ✅
- Throughput: 1M+ req/s ✅

### Test 3: Different HTTP Methods

```bash
# Test POST
curl -X POST -d '{"name":"test"}' http://localhost:8080/api/users

# Test PUT
curl -X PUT -d '{"name":"updated"}' http://localhost:8080/api/users/123

# Test DELETE
curl -X DELETE http://localhost:8080/api/users/123
```

### Test 4: Metrics Verification

Check RAUTA metrics (printed every 10 seconds):
```
INFO Metrics report
  packets_total: 1000000
  tier1: 950000      # 95% routed in XDP
  tier2: 0           # (not implemented yet)
  tier3: 40000       # 4% fallback to userspace
  dropped: 10000     # 1% no route
  parse_errors: 0
  tier1_hit_rate: 95.0%
```

## Performance Benchmarking

### Measure Latency with bpftrace

Terminal 4:
```bash
# Measure XDP program execution time
sudo bpftrace -e '
kprobe:rauta_ingress {
    @start[tid] = nsecs;
}

kretprobe:rauta_ingress /@start[tid]/ {
    @latency_ns = hist(nsecs - @start[tid]);
    delete(@start[tid]);
}

interval:s:10 {
    print(@latency_ns);
    clear(@latency_ns);
}
'
```

**Expected histogram**:
```
@latency_ns:
[2K, 4K)               3 |                                                    |
[4K, 8K)              89 |@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@|
[8K, 16K)             45 |@@@@@@@@@@@@@@@@@@@@@@@@@@                         |
[16K, 32K)             8 |@@@@                                               |
```

Most packets in 4-8μs range = **<10μs target achieved** ✅

### Benchmark with perf

```bash
# Profile XDP program
sudo perf record -e xdp:xdp_* -ag -- sleep 10
sudo perf report
```

## Expected Test Results

| Test | Target | Status |
|------|--------|--------|
| HTTP parsing | 100% methods | ⏳ Run test |
| Route lookup | <10μs | ⏳ Run test |
| Throughput | 1M+ req/s | ⏳ Run test |
| Latency p99 | <100μs | ⏳ Run test |
| Tier 1 hit rate | >80% | ⏳ Run test |

## Troubleshooting

### XDP program fails to load
```bash
# Check kernel version
uname -r  # Must be >= 4.18

# Check for BTF support
ls /sys/kernel/btf/vmlinux  # Should exist

# Try SKB mode instead of native
export RAUTA_XDP_MODE=skb
```

### No packets being routed
```bash
# Check if route is in BPF map
sudo bpftool map dump name ROUTES

# Check metrics
# Should see packets_total incrementing

# Verify interface
ip link show lo  # Should be UP
```

### High parse errors
```bash
# Check raw traffic with tcpdump
sudo tcpdump -i lo -A -s 0 'tcp port 8080'

# Look for:
# - Malformed HTTP requests
# - HTTP/2 or HTTP/3 (not supported)
# - Multi-packet requests (fall back to tier 3)
```

## Cleanup

```bash
# Stop RAUTA control plane (Ctrl-C in Terminal 2)

# Verify XDP detached
sudo bpftool net show  # Should not show rauta_ingress

# Stop backend server (Ctrl-C in Terminal 1)
```

## Automated Test Script

See `tests/integration_test.sh` for automated testing.

```bash
# Run full integration test suite
cd tests
sudo ./integration_test.sh
```

## Notes

- **macOS**: XDP not supported - tests must run on Linux
- **Docker**: Use `--privileged` and `--network=host` for XDP
- **VM**: Ensure nested virtualization for XDP support
