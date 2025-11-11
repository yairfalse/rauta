#!/bin/bash
# Gateway Performance Comparison Benchmark
# Compares RAUTA against NGINX Ingress and Envoy/Contour

set -e

cleanup() {
    jobs -p | xargs -r kill 2>/dev/null || true
}
trap cleanup EXIT
RESULTS_DIR="./benchmark-results"
mkdir -p "$RESULTS_DIR"

echo "=========================================="
echo "Gateway Performance Comparison Benchmark"
echo "=========================================="
echo ""

# Test configuration
DURATION="30s"
THREADS=8
CONNECTIONS=200

# Function to run wrk benchmark
run_benchmark() {
    local name=$1
    local url=$2
    local output_file="${RESULTS_DIR}/${name}-results.txt"

    echo "Testing: $name"
    echo "URL: $url"
    echo "Configuration: ${THREADS} threads, ${CONNECTIONS} connections, ${DURATION}"
    echo ""

    docker exec rauta-test-control-plane wrk \
        -t${THREADS} \
        -c${CONNECTIONS} \
        -d${DURATION} \
        --latency \
        "${url}" | tee "$output_file"

    echo ""
    echo "Results saved to: $output_file"
    echo "=========================================="
    echo ""
}

# Function to extract key metrics
extract_metrics() {
    local file=$1
    local name=$2

    local rps=$(grep "Requests/sec:" "$file" | awk '{print $2}')
    local p50=$(grep "50%" "$file" | awk '{print $2}')
    local p99=$(grep "99%" "$file" | awk '{print $2}')
    local total=$(grep "requests in" "$file" | awk '{print $1}')

    echo "$name,$rps,$p50,$p99,$total"
}

# Test 1: RAUTA (our gateway)
echo "=== Test 1: RAUTA Gateway ==="
run_benchmark "rauta" "http://localhost/api/test"

# Test 2: NGINX Ingress Controller (if installed)
echo "=== Test 2: NGINX Ingress Controller ==="
if kubectl get svc -n ingress-nginx ingress-nginx-controller &>/dev/null; then
    NGINX_IP=$(kubectl get svc -n ingress-nginx ingress-nginx-controller -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
    if [ -n "$NGINX_IP" ]; then
        run_benchmark "nginx-ingress" "http://${NGINX_IP}/api/test"
    else
        echo "NGINX Ingress LoadBalancer IP not available, using port-forward"
        kubectl port-forward -n ingress-nginx svc/ingress-nginx-controller 8080:80 &
        PF_PID=$!
        sleep 5
        run_benchmark "nginx-ingress" "http://localhost:8080/api/test"
        kill $PF_PID
    fi
else
    echo "NGINX Ingress Controller not installed. Skipping."
fi

# Test 3: Envoy/Contour (if installed)
echo "=== Test 3: Envoy (Contour) Gateway ==="
if kubectl get svc -n projectcontour envoy &>/dev/null; then
    ENVOY_IP=$(kubectl get svc -n projectcontour envoy -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
    if [ -n "$ENVOY_IP" ]; then
        run_benchmark "envoy-contour" "http://${ENVOY_IP}/api/test"
    else
        echo "Envoy LoadBalancer IP not available, using port-forward"
        kubectl port-forward -n projectcontour svc/envoy 8081:80 &
        PF_PID=$!
        sleep 5
        run_benchmark "envoy-contour" "http://localhost:8081/api/test"
        kill $PF_PID
    fi
else
    echo "Contour/Envoy not installed. Skipping."
fi

# Generate comparison report
echo ""
echo "=========================================="
echo "Benchmark Results Summary"
echo "=========================================="
echo ""
echo "Gateway,RPS,P50 Latency,P99 Latency,Total Requests"

for result in ${RESULTS_DIR}/*-results.txt; do
    if [ -f "$result" ]; then
        name=$(basename "$result" -results.txt)
        extract_metrics "$result" "$name"
    fi
done

echo ""
echo "Full results available in: $RESULTS_DIR/"
echo ""
