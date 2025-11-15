# RAUTA Integration Tests

## Prerequisites

### Any Platform (macOS, Linux, Windows)
```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install wrk for load testing (Linux/macOS)
# macOS:
brew install wrk

# Linux:
sudo apt-get install -y wrk

# Windows: Use WSL2 or download wrk from https://github.com/wg/wrk
```

### Build RAUTA
```bash
# Build control plane (pure Rust userspace)
cd rauta
cargo build --release

# Run tests
cargo test --all-features
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
# Set backend address
export RAUTA_BACKEND_ADDR="127.0.0.1:8080"

# Run RAUTA (no sudo needed - pure userspace)
./target/release/control
```

Expected output:
```
INFO RAUTA Control Plane starting...
INFO Pure Rust userspace HTTP proxy
INFO Test route added: GET /api/users -> 127.0.0.1:8080
INFO Listening on 0.0.0.0:8080
INFO Metrics endpoint: http://0.0.0.0:9090/metrics
INFO Control plane running. Press Ctrl-C to exit.
```

### 3. Verify Routing

Terminal 3:
```bash
# Check metrics endpoint
curl http://localhost:9090/metrics

# Should show Prometheus metrics:
# rauta_http_requests_total
# rauta_http_request_duration_seconds
# rauta_backend_health
```

## Integration Tests

### Test 1: HTTP GET Routing

```bash
# Send HTTP requests through RAUTA
curl -v http://localhost:8080/api/users

# Check routing metrics
curl http://localhost:9090/metrics | grep rauta_http_requests_total
```

### Test 2: HTTP POST with Body

```bash
# POST with JSON body
curl -X POST http://localhost:8080/api/users \
  -H "Content-Type: application/json" \
  -d '{"name":"test","email":"test@example.com"}'

# Verify body forwarded correctly
```

### Test 3: Header Forwarding

```bash
# Send request with custom headers
curl -H "X-Request-ID: 12345" \
     -H "Authorization: Bearer token123" \
     http://localhost:8080/api/test

# Verify headers preserved
```

### Test 4: Load Testing

```bash
# Basic load test (10 seconds)
wrk -t4 -c100 -d10s http://localhost:8080/api/users

# With latency histogram
wrk -t12 -c400 -d30s --latency http://localhost:8080/api/users
```

Expected results:
```
Running 30s test @ http://localhost:8080/api/users
  12 threads and 400 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     3.05ms    1.17ms  40.76ms   90.92%
    Req/Sec    10.88k     1.78k   92.92k    93.70%
  3,899,443 requests in 30.10s, 554.10MB read

Requests/sec: 129,530.83
Transfer/sec:     18.41MB
```

### Test 5: Graceful Shutdown

```bash
# Run shutdown test script
./scripts/test_graceful_shutdown.sh
```

Expected: Active connections drain before shutdown (30s timeout)

## Kubernetes Testing (Optional)

### Deploy to kind

```bash
# Create kind cluster
kind create cluster --name rauta-test

# Deploy RAUTA
kubectl apply -f deploy/

# Test Gateway API
kubectl apply -f examples/gateway-httproute.yaml

# Port-forward to test
kubectl port-forward -n rauta-system svc/rauta 8080:80

# Test routing
curl http://localhost:8080/api/test
```

## Performance Benchmarks

See `docs/PERF_RESULTS_HTTP2.md` for detailed performance results.

**Current baseline (M2 MacBook Pro):**
- HTTP/1.1: ~88,000 rps
- HTTP/2: ~129,000 rps
- Latency p99: <40ms

## Troubleshooting

### Port Already in Use

```bash
# Find process using port 8080
lsof -i :8080

# Kill process
kill -9 <PID>
```

### Backend Not Responding

```bash
# Check backend is running
curl http://localhost:8080

# Check RAUTA logs
RUST_LOG=debug ./target/release/control
```

### Connection Refused

```bash
# Verify RAUTA is listening
netstat -an | grep 8080

# Check firewall (Linux)
sudo iptables -L -n | grep 8080

# Check firewall (macOS)
sudo pfctl -s rules | grep 8080
```

## Automated Tests

Run all tests:
```bash
# Unit tests
cargo test --all-features

# Integration tests
cargo test --test '*'

# With coverage (requires cargo-tarpaulin)
cargo tarpaulin --all-features --workspace --timeout 120
```
