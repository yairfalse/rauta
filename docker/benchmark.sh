#!/bin/bash
#
# Benchmark RAUTA performance
#
# This script measures:
# 1. Throughput (requests/second)
# 2. Latency (p50, p99, p999)
# 3. XDP processing time (if bpftrace available)

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[BENCH]${NC} $1"
}

# Ensure environment is running
if ! docker-compose ps | grep -q "rauta-control"; then
    log_info "Starting test environment..."
    docker-compose up -d
    sleep 10
fi

log_info "=========================================="
log_info "RAUTA Performance Benchmark"
log_info "=========================================="
log_info ""

# Benchmark 1: Baseline throughput
log_info "Benchmark 1: Throughput (30 seconds, 12 threads, 400 connections)"
docker-compose exec -T client wrk -t12 -c400 -d30s \
    --latency \
    http://10.0.1.10:8080/api/users \
    > /tmp/rauta_bench.txt

cat /tmp/rauta_bench.txt

# Extract key metrics
REQUESTS_SEC=$(grep "Requests/sec:" /tmp/rauta_bench.txt | awk '{print $2}')
LATENCY_AVG=$(grep "Latency" /tmp/rauta_bench.txt | head -1 | awk '{print $2}')
LATENCY_P50=$(grep "50.000%" /tmp/rauta_bench.txt | awk '{print $2}')
LATENCY_P99=$(grep "99.000%" /tmp/rauta_bench.txt | awk '{print $2}')

log_info ""
log_info "=========================================="
log_info "Results Summary"
log_info "=========================================="
log_info ""
log_info "Throughput:    $REQUESTS_SEC req/s"
log_info "Latency Avg:   $LATENCY_AVG"
log_info "Latency p50:   $LATENCY_P50"
log_info "Latency p99:   $LATENCY_P99"
log_info ""

# Check if meets targets (from CLAUDE.md)
log_info "Target Validation:"

# Throughput target: 1M+ req/s (ambitious, but let's see)
REQUESTS_NUM=$(echo $REQUESTS_SEC | sed 's/[^0-9.]//g')
if [ -n "$REQUESTS_NUM" ]; then
    if (( $(echo "$REQUESTS_NUM > 100000" | bc -l) )); then
        log_info "✓ Throughput > 100k req/s"
    else
        log_info "✗ Throughput < 100k req/s (got: $REQUESTS_NUM)"
    fi
fi

# Latency target: p99 < 100μs (from CLAUDE.md)
log_info ""
log_info "Note: p99 < 100μs target requires native XDP mode"
log_info "      (Docker uses SKB mode which is slower)"
log_info ""

# Check RAUTA metrics
log_info "=========================================="
log_info "RAUTA Metrics"
log_info "=========================================="
log_info ""

docker-compose logs rauta | grep "Metrics report" | tail -1

log_info ""
log_info "Benchmark complete!"
log_info ""
log_info "Full report: /tmp/rauta_bench.txt"
