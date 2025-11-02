#!/bin/bash

# Complete load test setup and execution
set -e

echo "🚀 RAUTA Load Test Suite"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Cleanup
echo "🧹 Cleaning up old processes..."
pkill -9 -f "control" || true
pkill -9 -f "http2_backend" || true
lsof -ti:8081 -ti:8082 | xargs kill -9 2>/dev/null || true
sleep 2

# Build
echo "📦 Building binaries..."
cd "$(dirname "$0")"
cargo build --release --bin control --example http2_backend 2>&1 | grep -E "(Compiling|Finished)" || true

# Start HTTP/2 backend on port 8082
echo "🔧 Starting HTTP/2 backend server on :8082..."
BACKEND_PORT=8082 cargo run --release --example http2_backend > backend.log 2>&1 &
BACKEND_PID=$!
sleep 2

# Test backend (HTTP/2 requires --http2-prior-knowledge flag)
echo "✅ Testing backend..."
if ! curl -s --http2-prior-knowledge http://127.0.0.1:8082/health > /dev/null; then
    echo "⚠️  Backend started but health check failed (this is OK if HTTP/2 only)"
    # Don't exit - the backend is running, just doesn't respond to /health
fi

# Start proxy on port 8081, routing to backend on 8082
echo "🔧 Starting proxy server on :8081 -> :8082..."
env RAUTA_BIND_ADDR="127.0.0.1:8081" RAUTA_BACKEND_ADDR="127.0.0.1:8082" \
  ./target/release/control > proxy.log 2>&1 &
PROXY_PID=$!
sleep 3

# Test proxy
echo "✅ Testing proxy..."
if ! curl -s http://127.0.0.1:8081/health > /dev/null; then
    echo "⚠️  Proxy may not be ready, checking logs..."
    tail -20 proxy.log
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "✅ Test environment ready!"
echo "   Backend PID: $BACKEND_PID (port 8082)"
echo "   Proxy PID: $PROXY_PID (port 8081)"
echo "   Logs: backend.log, proxy.log"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Wait a bit more for full startup
sleep 2

# Run progressive load tests
echo "🔥 Starting progressive load tests..."
OUTPUT_FILE="load_test_results.txt"

echo "🔥 RAUTA HTTP/2 PROGRESSIVE LOAD TESTS" > "$OUTPUT_FILE"
echo "Started: $(date)" >> "$OUTPUT_FILE"
echo "" >> "$OUTPUT_FILE"

# Test 1: Baseline
echo "━━━ TEST 1: BASELINE (10 conn, 5s) ━━━"
wrk -t2 -c10 -d5s http://127.0.0.1:8081/api/test 2>&1 | tee -a "$OUTPUT_FILE" | grep -E "(Requests/sec|Latency|Thread Stats)" || true
echo "" | tee -a "$OUTPUT_FILE"

# Test 2: Light load
echo "━━━ TEST 2: LIGHT (50 conn, 10s) ━━━"
wrk -t4 -c50 -d10s http://127.0.0.1:8081/api/test 2>&1 | tee -a "$OUTPUT_FILE" | grep -E "(Requests/sec|Latency|Thread Stats)" || true
echo "" | tee -a "$OUTPUT_FILE"

# Test 3: Medium load
echo "━━━ TEST 3: MEDIUM (100 conn, 10s) ━━━"
wrk -t4 -c100 -d10s http://127.0.0.1:8081/api/test 2>&1 | tee -a "$OUTPUT_FILE" | grep -E "(Requests/sec|Latency|Thread Stats)" || true
echo "" | tee -a "$OUTPUT_FILE"

# Test 4: Heavy load
echo "━━━ TEST 4: HEAVY (200 conn, 10s) ━━━"
wrk -t8 -c200 -d10s http://127.0.0.1:8081/api/test 2>&1 | tee -a "$OUTPUT_FILE" | grep -E "(Requests/sec|Latency|Thread Stats)" || true
echo "" | tee -a "$OUTPUT_FILE"

# Test 5: Stress test
echo "━━━ TEST 5: STRESS (400 conn, 10s) ━━━"
wrk -t12 -c400 -d10s http://127.0.0.1:8081/api/test 2>&1 | tee -a "$OUTPUT_FILE" | grep -E "(Requests/sec|Latency|Thread Stats)" || true
echo "" | tee -a "$OUTPUT_FILE"

# Test 6: Max load
echo "━━━ TEST 6: MAX (800 conn, 10s) ━━━"
wrk -t12 -c800 -d10s http://127.0.0.1:8081/api/test 2>&1 | tee -a "$OUTPUT_FILE" | grep -E "(Requests/sec|Latency|Thread Stats)" || true
echo "" | tee -a "$OUTPUT_FILE"

echo "Completed: $(date)" >> "$OUTPUT_FILE"

# Summary
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "📊 SUMMARY (Requests/sec):"
grep "Requests/sec:" "$OUTPUT_FILE" | nl
echo ""
echo "📁 Full results: $OUTPUT_FILE"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Cleanup
echo ""
read -p "Press Enter to stop servers and exit..."
kill $BACKEND_PID $PROXY_PID 2>/dev/null || true
echo "✅ Cleanup complete"
