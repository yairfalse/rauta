#!/bin/bash
set -e

# RAUTA kind Deployment Script
# Deploys RAUTA Gateway API controller to a kind cluster

CLUSTER_NAME="rauta-test"
NAMESPACE="rauta-system"

echo "🚀 RAUTA kind Deployment"
echo "========================"
echo ""

# Check prerequisites
command -v kind >/dev/null 2>&1 || { echo "❌ kind not found. Install from https://kind.sigs.k8s.io/"; exit 1; }
command -v kubectl >/dev/null 2>&1 || { echo "❌ kubectl not found"; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "❌ docker not found"; exit 1; }

# Step 1: Create kind cluster
echo "📦 Step 1: Creating kind cluster..."
if kind get clusters | grep -q "^${CLUSTER_NAME}$"; then
    echo "⚠️  Cluster ${CLUSTER_NAME} already exists"
    read -p "Delete and recreate? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        kind delete cluster --name ${CLUSTER_NAME}
    else
        echo "Using existing cluster"
    fi
fi

if ! kind get clusters | grep -q "^${CLUSTER_NAME}$"; then
    kind create cluster --config deploy/kind-config.yaml --name ${CLUSTER_NAME}
    # Verify cluster creation succeeded
    if ! kind get clusters | grep -q "^${CLUSTER_NAME}$"; then
        echo "❌ Cluster creation failed"
        exit 1
    fi
fi

# Set kubectl context
kubectl cluster-info --context kind-${CLUSTER_NAME}
echo "✅ Cluster ready"
echo ""

# Step 2: Install Gateway API CRDs
echo "📚 Step 2: Installing Gateway API CRDs..."
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml
echo "✅ Gateway API CRDs installed"
echo ""

# Wait for CRDs to be established
echo "⏳ Waiting for CRDs to be ready..."
kubectl wait --for condition=established --timeout=60s crd/gatewayclasses.gateway.networking.k8s.io
kubectl wait --for condition=established --timeout=60s crd/gateways.gateway.networking.k8s.io
kubectl wait --for condition=established --timeout=60s crd/httproutes.gateway.networking.k8s.io
echo "✅ CRDs ready"
echo ""

# Step 3: Build RAUTA Docker image
echo "🔨 Step 3: Building RAUTA Docker image..."
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "❌ Not in a git repository"; exit 1; }
cd "$REPO_ROOT"
docker build -t rauta:latest -f docker/Dockerfile.prod .
echo "✅ Image built"
echo ""

# Step 4: Load image into kind
echo "📥 Step 4: Loading image into kind..."
kind load docker-image rauta:latest --name ${CLUSTER_NAME}
echo "✅ Image loaded"
echo ""

# Step 5: Deploy RAUTA
echo "🚀 Step 5: Deploying RAUTA DaemonSet..."
kubectl apply -f deploy/rauta-daemonset.yaml
echo "✅ RAUTA deployed"
echo ""

# Wait for RAUTA to be ready
echo "⏳ Waiting for RAUTA pods to be ready..."
kubectl wait --for=condition=ready pod -l app=rauta -n ${NAMESPACE} --timeout=120s
echo "✅ RAUTA pods ready"
echo ""

# Step 6: Deploy demo backend
echo "🎯 Step 6: Deploying demo backend application..."
kubectl apply -f deploy/demo-backend.yaml
echo "✅ Demo backend deployed"
echo ""

# Wait for demo backends
echo "⏳ Waiting for demo pods to be ready..."
kubectl wait --for=condition=ready pod -l app=echo -n demo --timeout=120s
echo "✅ Demo pods ready"
echo ""

# Step 7: Deploy Gateway API resources
echo "🌐 Step 7: Creating GatewayClass and HTTPRoute..."
kubectl apply -f deploy/gateway-api.yaml
echo "✅ Gateway API resources created"
echo ""

# Wait for Gateway to be ready
echo "⏳ Waiting for Gateway to be ready..."
kubectl wait --for=condition=Programmed gateway/rauta-gateway -n demo --timeout=60s || echo "⚠️  Gateway not ready yet (check manually)"
echo ""

# Step 8: Show status
echo "📊 Deployment Status"
echo "===================="
echo ""

echo "RAUTA Pods:"
kubectl get pods -n ${NAMESPACE} -o wide
echo ""

echo "Demo Pods:"
kubectl get pods -n demo -o wide
echo ""

echo "GatewayClass:"
kubectl get gatewayclass rauta -o wide
echo ""

echo "Gateway:"
kubectl get gateway rauta-gateway -n demo -o wide
echo ""

echo "HTTPRoute:"
kubectl get httproute echo-route -n demo -o wide
echo ""

echo "EndpointSlices (should show 3 stable + 1 canary backends):"
kubectl get endpointslices -n demo -l kubernetes.io/service-name=echo-stable
kubectl get endpointslices -n demo -l kubernetes.io/service-name=echo-canary
echo ""

# Step 9: Test routing
echo "🧪 Testing Routing"
echo "=================="
echo ""

echo "Test 1: Basic routing to /"
for i in {1..5}; do
    curl -s -H "Host: echo.local" http://localhost:8080/ || echo "Failed"
done
echo ""

echo "Test 2: Canary routing to /api (should see ~90% v1, ~10% v2)"
echo "Making 20 requests to /api..."
for i in {1..20}; do
    curl -s -H "Host: echo.local" http://localhost:8080/api | grep -o "v[12]" || echo "Failed"
done | sort | uniq -c
echo ""

echo "Test 3: Metrics endpoint"
curl -s http://localhost:9090/metrics | grep rauta_requests_total | head -5
echo ""

echo "✅ Deployment complete!"
echo ""
echo "Next steps:"
echo "  - View RAUTA logs: kubectl logs -n ${NAMESPACE} -l app=rauta -f"
echo "  - Test routing: curl -H 'Host: echo.local' http://localhost:8080/api"
echo "  - View metrics: curl http://localhost:9090/metrics"
echo "  - Scale backends: kubectl scale deployment echo-v1 --replicas=5 -n demo"
echo "  - Delete cluster: kind delete cluster --name ${CLUSTER_NAME}"
echo ""
