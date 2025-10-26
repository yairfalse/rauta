#!/bin/bash
#
# Run RAUTA integration tests in Docker
#
# This script:
# 1. Builds the Docker image (if needed)
# 2. Starts the test environment (backend + rauta + client)
# 3. Runs integration tests
# 4. Reports results

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[TEST]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[TEST]${NC} $1"
}

log_error() {
    echo -e "${RED}[TEST]${NC} $1"
}

# Check if image exists
if ! docker images rauta:latest | grep -q rauta; then
    log_info "RAUTA image not found. Building..."
    ./docker/build.sh
fi

# Start the test environment
log_info "Starting test environment..."
docker-compose up -d

# Wait for services to be ready
log_info "Waiting for services to start..."
sleep 10

# Check backend health
log_info "Checking backend health..."
if docker-compose exec -T backend curl -sf http://localhost:8080 > /dev/null; then
    log_info "✓ Backend is healthy"
else
    log_error "✗ Backend failed to start"
    docker-compose logs backend
    exit 1
fi

# Check RAUTA control plane
log_info "Checking RAUTA control plane..."
if docker-compose ps rauta | grep -q "Up"; then
    log_info "✓ RAUTA control plane is running"
else
    log_error "✗ RAUTA control plane failed to start"
    docker-compose logs rauta
    exit 1
fi

# Run basic connectivity test
log_info "Test 1: Basic HTTP connectivity"
if docker-compose exec -T client curl -sf http://10.0.1.10:8080 > /dev/null; then
    log_info "✓ HTTP connectivity works"
else
    log_error "✗ HTTP connectivity failed"
fi

# Run load test with wrk
log_info "Test 2: Load testing with wrk (10 seconds)"
docker-compose exec -T client wrk -t4 -c100 -d10s http://10.0.1.10:8080/api/users

# Check RAUTA metrics
log_info "Test 3: RAUTA metrics"
log_info "Checking control plane logs for metrics..."
docker-compose logs rauta | grep -A 5 "Metrics report" | tail -10

# Test different HTTP methods
log_info "Test 4: HTTP methods"
for method in GET POST PUT DELETE; do
    if docker-compose exec -T client curl -sf -X $method http://10.0.1.10:8080/test > /dev/null 2>&1; then
        log_info "✓ $method method works"
    else
        log_warn "✗ $method method failed (may be expected)"
    fi
done

# Summary
log_info ""
log_info "========================================"
log_info "Test Summary"
log_info "========================================"
log_info ""
log_info "All tests completed!"
log_info ""
log_info "View logs:"
log_info "  Backend:  docker-compose logs backend"
log_info "  RAUTA:    docker-compose logs rauta"
log_info "  Client:   docker-compose logs client"
log_info ""
log_info "Cleanup:"
log_info "  docker-compose down"
