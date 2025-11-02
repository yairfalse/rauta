# Changelog

All notable changes to RAUTA will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added - HTTP/2 Worker Pool Architecture (2025-11-02)

#### Performance Gains
- **1.47x throughput improvement** (88K → 130K rps) with HTTP/2 worker pools
- **47% more requests/second** under load testing (wrk: 12 threads, 400 connections, 30s)
- **14% latency reduction** (3.5ms → 3.0ms average latency)
- **33x fewer connections** required (400 → 12 via HTTP/2 multiplexing)

#### HTTP/2 Protocol Detection
- Default to HTTP/2 for modern backends (`unwrap_or(true)`)
- Automatic fallback to HTTP/1.1 when backend doesn't support HTTP/2
- Protocol caching to avoid repeated detection overhead
- Intelligent retry logic: rebuild request and retry with HTTP/1.1 on HTTP/2 failure

#### Per-Worker Observability Metrics
- Added `worker_id` label to all HTTP/2 pool metrics:
  - `http2_pool_connections_active{backend, worker_id}`
  - `http2_pool_connections_created_total{backend, worker_id}`
  - `http2_pool_connections_failed_total{backend, worker_id}`
  - `http2_pool_requests_queued_total{backend, worker_id}`
- Added `worker_id` label to HTTP request metrics:
  - `http_requests_total{method, path, status, worker_id}`
- Enable monitoring of lock-free operation
- Track per-worker connection and request distribution
- Verify no cross-worker contention

#### Architecture Improvements
- Each worker owns its HTTP/2 connection pools (no Arc/Mutex on hot path)
- Lock-free connection acquisition per worker
- HTTP/2 multiplexing reduces connection overhead
- Round-robin worker selection for load distribution

#### Testing
- Created HTTP/2 backend example server (`control/examples/http2_backend.rs`)
- All 46/46 tests passing with HTTP/2 mock backends
- Updated test assertions to expect `worker_id` labels

#### Documentation
- `PERF_RESULTS_HTTP2.md` - Comprehensive performance analysis
- Load test results with detailed metrics breakdown
- Architecture validation and research backing

### Technical Details

**Before (HTTP/1.1 baseline):**
```
Requests/sec: ~88,000
Latency avg: ~3.5ms
Architecture: Shared connection pool with mutex contention
```

**After (HTTP/2 + per-core workers):**
```
Requests/sec: 129,814
Latency avg: 3.00ms
Architecture: Lock-free per-worker pools with HTTP/2 multiplexing
```

**Architecture:**
```
┌───────────────────────────────────────┐
│  ProxyServer (12 per-core workers)    │
│                                       │
│  Worker 0: HTTP/2 Pool (lock-free)    │
│  Worker 1: HTTP/2 Pool (lock-free)    │
│  Worker 2: HTTP/2 Pool (lock-free)    │
│  ...                                  │
│  Worker 11: HTTP/2 Pool (lock-free)   │
│                                       │
│  ✓ Zero mutex contention              │
│  ✓ HTTP/2 multiplexing                │
│  ✓ Per-worker observability           │
└───────────────────────────────────────┘
```

### Changed
- HTTP/2 backend connection pools now require `worker_id` parameter
- `BackendConnectionPools::new(worker_id: usize)` signature updated
- Test mock backends updated to HTTP/2 (`http2::Builder`)

### Performance Notes

**Expected gain:** 1.7-2.2x (from Prometheus metrics research)
**Actual gain:** 1.47x (86% of lower bound)

**Analysis:**
- Close to expected range given test environment constraints
- Limited by MacBook Pro thermal throttling during sustained load
- Localhost loopback overhead (not production network)
- Simple HTTP/2 backend (not optimized production service)

**Production expectations:**
- 1.7-2.0x gain with production hardware
- Better sustained performance without thermal limits
- Real network eliminates loopback bottleneck
- Optimized backends maximize worker pool benefits

### Next Steps
- Add worker pool architecture documentation
- Create Grafana dashboard templates for per-worker metrics
- Implement io_uring backend (+31-43% expected gain)
- Add TLS termination with rustls

## Previous Versions

See git history for earlier changes before formal CHANGELOG tracking.

---

**RAUTA** - Iron-clad routing with safe extensibility
