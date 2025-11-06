# RAUTA Kubernetes Deployment

Deploy RAUTA Gateway API controller to a kind cluster for local testing.

## Quick Start

**One-command deployment:**

```bash
./deploy/deploy-to-kind.sh
```

This automated script will:
1. Create a 3-node kind cluster (1 control-plane, 2 workers)
2. Install Gateway API v1.2.0 CRDs
3. Build RAUTA Docker image
4. Deploy RAUTA as a DaemonSet
5. Deploy demo backend (echo service with stable + canary versions)
6. Create GatewayClass, Gateway, and HTTPRoute
7. Test routing with curl

## Prerequisites

- Docker Desktop running
- `kind` installed: https://kind.sigs.k8s.io/docs/user/quick-start/#installation
- `kubectl` installed
- `curl` for testing

## Deployment Files

```
deploy/
├── kind-config.yaml           # kind cluster config (3 nodes, port mappings)
├── rauta-daemonset.yaml       # RAUTA deployment (namespace, RBAC, DaemonSet)
├── demo-backend.yaml          # Demo echo service (stable + canary)
├── gateway-api.yaml           # GatewayClass + Gateway + HTTPRoute
├── deploy-to-kind.sh          # Automated deployment script
└── README.md                  # This file
```

## Manual Deployment

If you prefer step-by-step deployment:

### 1. Create kind cluster

```bash
kind create cluster --config deploy/kind-config.yaml --name rauta-test
```

### 2. Install Gateway API CRDs

```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml
```

### 3. Build and load RAUTA image

```bash
docker build -t rauta:latest -f docker/Dockerfile.prod .
kind load docker-image rauta:latest --name rauta-test
```

### 4. Deploy RAUTA

```bash
kubectl apply -f deploy/rauta-daemonset.yaml
kubectl wait --for=condition=ready pod -l app=rauta -n rauta-system --timeout=120s
```

### 5. Deploy demo backend

```bash
kubectl apply -f deploy/demo-backend.yaml
kubectl wait --for=condition=ready pod -l app=echo -n demo --timeout=120s
```

### 6. Create Gateway resources

```bash
kubectl apply -f deploy/gateway-api.yaml
```

## Testing

### Basic routing

```bash
# Test root path (stable only)
curl -H "Host: echo.local" http://localhost:8080/

# Test /api path (90% stable, 10% canary)
for i in {1..20}; do
  curl -s -H "Host: echo.local" http://localhost:8080/api
done
```

### View metrics

```bash
curl http://localhost:9090/metrics | grep rauta
```

### View logs

```bash
# RAUTA controller logs
kubectl logs -n rauta-system -l app=rauta -f

# Demo backend logs
kubectl logs -n demo -l app=echo -f
```

### Check resources

```bash
# GatewayClass status
kubectl get gatewayclass rauta -o yaml

# Gateway status
kubectl get gateway rauta-gateway -n demo -o yaml

# HTTPRoute status
kubectl get httproute echo-route -n demo -o yaml

# EndpointSlices (dynamic backends)
kubectl get endpointslices -n demo -l kubernetes.io/service-name=echo-stable
kubectl get endpointslices -n demo -l kubernetes.io/service-name=echo-canary
```

## Validate Features

### 1. EndpointSlice Resolution (Dynamic Backends)

```bash
# Scale stable version
kubectl scale deployment echo-v1 --replicas=5 -n demo

# Watch RAUTA logs - should see EndpointSlice update
kubectl logs -n rauta-system -l app=rauta -f | grep endpointslice

# Make requests - should distribute across all 5 pods
for i in {1..10}; do curl -s -H "Host: echo.local" http://localhost:8080/; done
```

### 2. Weighted Backends (Canary Deployment)

```bash
# Make 100 requests to /api
for i in {1..100}; do
  curl -s -H "Host: echo.local" http://localhost:8080/api | grep -o "v[12]"
done | sort | uniq -c

# Expected output (approximately):
#  90 v1  (90% to stable)
#  10 v2  (10% to canary)
```

### 3. Passive Health Checking

**Note**: Currently wired up but needs backend that returns 5xx errors.

Simulate unhealthy backend:
```bash
# Deploy a backend that returns 500s
kubectl run unhealthy --image=hashicorp/http-echo --port=5678 \
  -- -text="500" -listen=:5678 -n demo

# Add to HTTPRoute backendRefs
# After 50% of requests fail, RAUTA should exclude it
```

### 4. Connection Draining

**Note**: Currently implemented but needs integration with EndpointSlice watcher.

Simulate Pod deletion:
```bash
# Delete a Pod
kubectl delete pod -n demo -l app=echo,version=v1 --force

# RAUTA should:
# 1. Mark backend for draining (30s grace period)
# 2. Route NEW connections to other backends
# 3. Allow EXISTING connections to finish
```

## Architecture in kind

```
┌─────────────────────────────────────────┐
│     kind Cluster (rauta-test)           │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │  Control Plane Node               │  │
│  │  - Gateway API CRDs               │  │
│  │  - RAUTA DaemonSet (1 pod)        │  │
│  │    * Listens on :80, :443, :9090  │  │
│  │    * Watches GatewayClass, etc    │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │  Worker Node 1                    │  │
│  │  - RAUTA DaemonSet (1 pod)        │  │
│  │  - Demo Pods (echo-v1, echo-v2)   │  │
│  └───────────────────────────────────┘  │
│                                         │
│  ┌───────────────────────────────────┐  │
│  │  Worker Node 2                    │  │
│  │  - RAUTA DaemonSet (1 pod)        │  │
│  │  - Demo Pods (echo-v1, echo-v2)   │  │
│  └───────────────────────────────────┘  │
└─────────────────────────────────────────┘
         │
         │ Port forwarding
         ▼
    localhost:8080 → RAUTA :80
    localhost:9090 → RAUTA :9090 (metrics)
```

## Troubleshooting

### RAUTA pods not starting

```bash
# Check pod status
kubectl get pods -n rauta-system -o wide

# Check pod events
kubectl describe pod -n rauta-system -l app=rauta

# Check logs
kubectl logs -n rauta-system -l app=rauta
```

### No routing happening

```bash
# Verify GatewayClass is accepted
kubectl get gatewayclass rauta -o jsonpath='{.status.conditions}'

# Verify Gateway is ready
kubectl get gateway rauta-gateway -n demo -o jsonpath='{.status.conditions}'

# Verify HTTPRoute is accepted
kubectl get httproute echo-route -n demo -o jsonpath='{.status.conditions}'
```

### Docker build fails

```bash
# Check Docker is running
docker ps

# Build manually to see errors
docker build -t rauta:latest -f docker/Dockerfile.prod .
```

## Cleanup

```bash
# Delete the entire cluster
kind delete cluster --name rauta-test

# Just restart RAUTA
kubectl rollout restart daemonset rauta -n rauta-system

# Remove demo backends
kubectl delete namespace demo
```

## Next Steps

After validating basic routing:

1. **Wire up passive health checking** - Connect `record_backend_response()` in proxy server
2. **Wire up connection draining** - Call `drain_backend()` from EndpointSlice watcher
3. **Test with real failures** - Deploy backend that returns 5xx, verify exclusion
4. **Test rolling updates** - Scale down Pods, verify zero connection errors
5. **Load testing** - Use `wrk` to generate 10K rps and measure latency
6. **WASM plugins** - Add JWT auth plugin and test extensibility

## Documentation

- [RAUTA README](../README.md) - Project overview
- [Development Guide](../DEVELOPMENT.md) - Build from source
- [Gateway API Docs](https://gateway-api.sigs.k8s.io/) - Gateway API spec
- [kind Docs](https://kind.sigs.k8s.io/) - Local Kubernetes clusters
