# RAUTA Kubernetes Manifests

Kubernetes deployment manifests for RAUTA Gateway API controller.

## Architecture

RAUTA runs as a **DaemonSet with hostNetwork** (see [ADR 001](../docs/adr/001-hostnetwork-daemonset-architecture.md)):
- Direct network access for high performance (129K+ req/s)
- One controller pod per node
- Binds to host ports 8080 (HTTP) and 8443 (HTTPS)

## Quick Start

### 1. Install Gateway API CRDs

```bash
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml
```

### 2. Build and Load RAUTA Image

```bash
# Build container
docker build -t rauta:latest .

# Load into kind (if using kind)
kind load docker-image rauta:latest --name <cluster-name>
```

### 3. Deploy RAUTA

```bash
# Deploy core components
kubectl apply -f manifests/00-namespace.yaml
kubectl apply -f manifests/01-rbac.yaml
kubectl apply -f manifests/02-gatewayclass.yaml
kubectl apply -f manifests/03-daemonset.yaml
kubectl apply -f manifests/04-service.yaml

# Or all at once
kubectl apply -f manifests/
```

### 4. Verify Deployment

```bash
# Check pods
kubectl get pods -n rauta-system

# Check GatewayClass
kubectl get gatewayclass

# Check logs
kubectl logs -n rauta-system -l app.kubernetes.io/name=rauta -f
```

## Example Usage

Deploy example Gateway and HTTPRoute:

```bash
kubectl apply -f manifests/examples/
```

Test the route:

```bash
# Get node IP (if using kind/minikube)
NODE_IP=$(kubectl get nodes -o jsonpath='{.items[0].status.addresses[?(@.type=="InternalIP")].address}')

# Test route
curl -H "Host: example.com" http://$NODE_IP:8080/api
```

## Configuration

### Environment Variables

Configure RAUTA via environment variables in the DaemonSet:

```yaml
env:
  - name: RUST_LOG
    value: "info,control=debug"  # Log level
  - name: RAUTA_CONTROLLER_NAME
    value: "rauta.io/gateway-controller"
  - name: RAUTA_GATEWAY_CLASS
    value: "rauta"
```

### Health Checking

Enable active health checking (disabled by default):

```yaml
env:
  - name: RAUTA_HEALTH_CHECK_ENABLED
    value: "true"
  - name: RAUTA_HEALTH_CHECK_INTERVAL_SECS
    value: "5"
```

See [ADR 002](../docs/adr/002-active-health-checking-observability.md) for details.

## Monitoring

### Prometheus Metrics

RAUTA exposes Prometheus metrics on port 9090:

```bash
# Port-forward to access metrics
kubectl port-forward -n rauta-system svc/rauta-metrics 9090:9090

# View metrics
curl http://localhost:9090/metrics
```

**Key Metrics:**
- `rauta_requests_total` - Total requests processed
- `rauta_backend_health_status` - Backend health (1=healthy, 0=unhealthy)
- `http2_pool_connections_active` - Active HTTP/2 connections
- `rauta_routing_duration_seconds` - Request routing latency

## Troubleshooting

### Pods Not Starting

```bash
# Check pod status
kubectl describe pods -n rauta-system -l app.kubernetes.io/name=rauta

# Check logs
kubectl logs -n rauta-system -l app.kubernetes.io/name=rauta
```

### Port Conflicts

If ports 8080/8443 are already in use:

```yaml
# Edit manifests/03-daemonset.yaml
ports:
  - name: http
    containerPort: 8080
    hostPort: 9080  # Change to available port
```

### Image Pull Errors

For local development with kind:

```bash
# Rebuild and reload image
docker build -t rauta:latest .
kind load docker-image rauta:latest --name <cluster-name>

# Restart pods
kubectl rollout restart daemonset/rauta-controller -n rauta-system
```

## Uninstall

```bash
# Delete all RAUTA resources
kubectl delete -f manifests/

# Delete Gateway API CRDs (optional)
kubectl delete -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.2.0/standard-install.yaml
```

## Files

- `00-namespace.yaml` - rauta-system namespace
- `01-rbac.yaml` - ServiceAccount, ClusterRole, ClusterRoleBinding
- `02-gatewayclass.yaml` - GatewayClass definition
- `03-daemonset.yaml` - RAUTA controller DaemonSet
- `04-service.yaml` - Metrics service
- `examples/` - Example Gateway and HTTPRoute

## References

- [Gateway API Docs](https://gateway-api.sigs.k8s.io/)
- [ADR 001: hostNetwork DaemonSet Architecture](../docs/adr/001-hostnetwork-daemonset-architecture.md)
- [ADR 002: Active Health Checking](../docs/adr/002-active-health-checking-observability.md)
