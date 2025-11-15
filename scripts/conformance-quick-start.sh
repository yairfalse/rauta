#!/bin/bash
set -euo pipefail

CLUSTER_NAME="rauta-conformance"

echo "🚀 Gateway API Conformance Quick Start"
echo "   This script will:"
echo "   1. Setup conformance test suite"
echo "   2. Create kind cluster"
echo "   3. Build and deploy RAUTA"
echo "   4. Run conformance tests"
echo ""

read -p "Continue? (y/n) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    exit 0
fi

# Step 1: Setup
echo "📦 Step 1/4: Setting up conformance test suite..."
./scripts/setup-conformance.sh

# Step 2: Create cluster
echo ""
echo "🏗️  Step 2/4: Creating kind cluster..."
if kind get clusters | grep -q "^$CLUSTER_NAME$"; then
    echo "⚠️  Cluster '$CLUSTER_NAME' already exists"
    read -p "Delete and recreate? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        kind delete cluster --name "$CLUSTER_NAME"
        kind create cluster --name "$CLUSTER_NAME"
    fi
else
    kind create cluster --name "$CLUSTER_NAME"
fi

# Step 3: Build and deploy RAUTA
echo ""
echo "🦀 Step 3/4: Building and deploying RAUTA..."
cargo build --release

echo "   Installing Gateway API CRDs..."
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml

echo "   Deploying RAUTA..."
if [ -d "manifests" ]; then
    kubectl apply -f manifests/
else
    echo "⚠️  No manifests/ directory found. You'll need to create deployment manifests."
    echo "   For now, creating minimal GatewayClass..."
    kubectl apply -f - <<EOF
apiVersion: gateway.networking.k8s.io/v1
kind: GatewayClass
metadata:
  name: rauta
spec:
  controllerName: rauta.io/gateway-controller
EOF
fi

# Give RAUTA time to start
echo "   Waiting for RAUTA to be ready..."
sleep 5

# Step 4: Run tests
echo ""
echo "🧪 Step 4/4: Running conformance tests..."
./scripts/run-conformance.sh

echo ""
echo "🎉 Done!"
echo "   Cluster: $CLUSTER_NAME"
echo "   To cleanup: kind delete cluster --name $CLUSTER_NAME"
