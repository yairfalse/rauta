#!/bin/bash
set -euo pipefail

echo "🔧 Setting up Gateway API Conformance Tests..."

# Check prerequisites
command -v kind >/dev/null 2>&1 || { echo "❌ kind not found. Install: https://kind.sigs.k8s.io/docs/user/quick-start/#installation"; exit 1; }
command -v kubectl >/dev/null 2>&1 || { echo "❌ kubectl not found. Install: https://kubernetes.io/docs/tasks/tools/"; exit 1; }
command -v go >/dev/null 2>&1 || { echo "❌ go not found. Install: https://go.dev/doc/install"; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "❌ docker not found. Install: https://docs.docker.com/get-docker/"; exit 1; }

echo "✅ Prerequisites satisfied"

# Clone gateway-api repo if not exists
GATEWAY_API_DIR="${GATEWAY_API_DIR:-/tmp/gateway-api}"
if [ ! -d "$GATEWAY_API_DIR" ]; then
    echo "📦 Cloning gateway-api repository..."
    git clone https://github.com/kubernetes-sigs/gateway-api "$GATEWAY_API_DIR"
else
    echo "✅ gateway-api repository already cloned"
    cd "$GATEWAY_API_DIR"
    git pull origin main
fi

# Install Go dependencies
echo "📦 Installing Go test dependencies..."
cd "$GATEWAY_API_DIR/conformance"
go mod download

echo "✅ Setup complete!"
echo ""
echo "Next steps:"
echo "  1. Create test cluster: kind create cluster --name rauta-conformance"
echo "  2. Build RAUTA: cargo build --release"
echo "  3. Deploy RAUTA: kubectl apply -f manifests/"
echo "  4. Run tests: ./scripts/run-conformance.sh"
