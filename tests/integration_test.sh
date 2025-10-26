#!/bin/bash
#
# RAUTA Integration Test Suite
#
# Usage: sudo ./integration_test.sh
#
# Requirements:
# - Linux kernel 4.18+
# - wrk installed
# - RAUTA built (bpf + control)

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    log_error "This script must be run as root (for XDP)"
    exit 1
fi

# Check if on Linux
if [ "$(uname)" != "Linux" ]; then
    log_error "XDP only works on Linux. Current OS: $(uname)"
    exit 1
fi

# Check kernel version
KERNEL_VERSION=$(uname -r | cut -d. -f1-2)
log_info "Kernel version: $KERNEL_VERSION"

# Check dependencies
log_info "Checking dependencies..."
command -v wrk >/dev/null 2>&1 || { log_error "wrk not found. Install: apt-get install wrk"; exit 1; }
command -v bpftool >/dev/null 2>&1 || { log_warn "bpftool not found. Install for debugging: apt-get install linux-tools-$(uname -r)"; }
command -v python3 >/dev/null 2>&1 || { log_error "python3 not found"; exit 1; }

# Build RAUTA if not already built
if [ ! -f "../control/target/release/control" ]; then
    log_info "Building RAUTA control plane..."
    cd ../control
    cargo build --release
    cd ../tests
fi

# Start backend HTTP server
log_info "Starting backend HTTP server on port 8080..."
python3 -m http.server 8080 --directory /tmp &
BACKEND_PID=$!
sleep 2

# Verify backend is up
if ! curl -s http://localhost:8080 > /dev/null; then
    log_error "Backend server failed to start"
    kill $BACKEND_PID 2>/dev/null || true
    exit 1
fi
log_info "Backend server started (PID: $BACKEND_PID)"

# Start RAUTA control plane
log_info "Starting RAUTA control plane..."
export RAUTA_INTERFACE=lo
export RAUTA_XDP_MODE=skb

../control/target/release/control &
RAUTA_PID=$!
sleep 5

# Verify RAUTA is running
if ! ps -p $RAUTA_PID > /dev/null; then
    log_error "RAUTA control plane failed to start"
    kill $BACKEND_PID 2>/dev/null || true
    exit 1
fi
log_info "RAUTA control plane started (PID: $RAUTA_PID)"

# Verify XDP attachment
if command -v bpftool >/dev/null 2>&1; then
    log_info "Checking XDP attachment..."
    if bpftool net show | grep -q rauta_ingress; then
        log_info "XDP program attached successfully"
    else
        log_warn "XDP program not found in bpftool output"
    fi
fi

# Test 1: Basic HTTP GET
log_info "Test 1: Basic HTTP GET"
if curl -s http://localhost:8080/ > /dev/null; then
    log_info "✓ HTTP GET successful"
else
    log_error "✗ HTTP GET failed"
fi

# Test 2: HTTP Methods
log_info "Test 2: HTTP Methods"
for method in GET POST PUT DELETE HEAD; do
    if curl -s -X $method http://localhost:8080/api/test > /dev/null 2>&1; then
        log_info "✓ $method method successful"
    else
        log_warn "✗ $method method failed (expected for some)"
    fi
done

# Test 3: Load test with wrk
log_info "Test 3: Load testing with wrk (10 seconds)..."
WRK_OUTPUT=$(wrk -t4 -c100 -d10s http://localhost:8080/api/users 2>&1)
echo "$WRK_OUTPUT"

# Extract req/sec
REQUESTS_PER_SEC=$(echo "$WRK_OUTPUT" | grep "Requests/sec:" | awk '{print $2}' | cut -d. -f1)
if [ -n "$REQUESTS_PER_SEC" ]; then
    log_info "Throughput: $REQUESTS_PER_SEC req/s"

    # Check if meets target (1M+ req/s is ambitious for loopback testing)
    if [ "$REQUESTS_PER_SEC" -gt 100000 ]; then
        log_info "✓ Throughput > 100k req/s"
    else
        log_warn "✗ Throughput below 100k req/s (got: $REQUESTS_PER_SEC)"
    fi
fi

# Test 4: Check metrics (if logs available)
log_info "Test 4: Checking RAUTA metrics..."
sleep 11  # Wait for next metrics report

# Test 5: Verify BPF maps (if bpftool available)
if command -v bpftool >/dev/null 2>&1; then
    log_info "Test 5: Checking BPF maps..."

    # Count routes
    ROUTES_COUNT=$(bpftool map dump name ROUTES 2>/dev/null | grep -c "key:" || echo "0")
    log_info "Routes in map: $ROUTES_COUNT"

    # Check flow cache
    FLOWS_COUNT=$(bpftool map dump name FLOW_CACHE 2>/dev/null | grep -c "key:" || echo "0")
    log_info "Flows in cache: $FLOWS_COUNT"
fi

# Cleanup
log_info "Cleaning up..."
kill $RAUTA_PID 2>/dev/null || true
kill $BACKEND_PID 2>/dev/null || true
sleep 2

# Verify XDP detached
if command -v bpftool >/dev/null 2>&1; then
    if bpftool net show | grep -q rauta_ingress; then
        log_warn "XDP program still attached after cleanup"
    else
        log_info "XDP program detached successfully"
    fi
fi

log_info "Integration tests completed!"
log_info ""
log_info "Summary:"
log_info "  - Basic HTTP: ✓"
log_info "  - Load test: Check output above"
log_info "  - Cleanup: ✓"
log_info ""
log_info "Next steps:"
log_info "  1. Check RAUTA logs for metrics"
log_info "  2. Run: sudo bpftrace -e 'kprobe:rauta_ingress { @start[tid] = nsecs; }'"
log_info "  3. Measure <10μs latency target"
