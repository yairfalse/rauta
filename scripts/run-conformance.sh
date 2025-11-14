#!/bin/bash
set -euo pipefail

GATEWAY_API_DIR="${GATEWAY_API_DIR:-/tmp/gateway-api}"
GATEWAY_CLASS="${GATEWAY_CLASS:-rauta}"
REPORT_OUTPUT="${REPORT_OUTPUT:-/tmp/rauta-conformance-report.yaml}"
SPECIFIC_TEST="${1:-}"

echo "🧪 Running Gateway API Conformance Tests..."
echo "   Gateway Class: $GATEWAY_CLASS"
echo "   Report: $REPORT_OUTPUT"

# Check if gateway-api repo exists
if [ ! -d "$GATEWAY_API_DIR" ]; then
    echo "❌ gateway-api repository not found. Run ./scripts/setup-conformance.sh first"
    exit 1
fi

# Check if cluster exists
if ! kubectl cluster-info &>/dev/null; then
    echo "❌ No Kubernetes cluster found. Create one with: kind create cluster --name rauta-conformance"
    exit 1
fi

# Check if RAUTA is deployed
if ! kubectl get gatewayclass "$GATEWAY_CLASS" &>/dev/null; then
    echo "❌ GatewayClass '$GATEWAY_CLASS' not found. Deploy RAUTA first: kubectl apply -f manifests/"
    exit 1
fi

echo "✅ Cluster ready, RAUTA deployed"

cd "$GATEWAY_API_DIR/conformance"

if [ -n "$SPECIFIC_TEST" ]; then
    echo "🎯 Running specific test: $SPECIFIC_TEST"
    go test -v -run "$SPECIFIC_TEST" \
        -gateway-class="$GATEWAY_CLASS"
else
    echo "🎯 Running full conformance suite..."
    go test -v \
        -gateway-class="$GATEWAY_CLASS" \
        -supported-features=Gateway,HTTPRoute \
        -report-output="$REPORT_OUTPUT" \
        -timeout=30m
fi

TEST_RESULT=$?

if [ $TEST_RESULT -eq 0 ]; then
    echo ""
    echo "✅ Conformance tests PASSED!"
    if [ -f "$REPORT_OUTPUT" ]; then
        echo "📄 Report saved to: $REPORT_OUTPUT"
    fi
else
    echo ""
    echo "❌ Conformance tests FAILED"
    echo "   Check logs above for details"
    exit 1
fi
