# Skaffold Development Workflow - RAUTA

Fast iteration workflow for RAUTA Gateway API controller development.

---

## Prerequisites

```bash
# Install Skaffold
brew install skaffold  # macOS
# or: https://skaffold.dev/docs/install/

# Install Kind (if not already)
brew install kind

# Verify installation
skaffold version
kind version
```

---

## Quick Start

### 1. Create Kind Cluster

```bash
# Create cluster with Gateway API CRDs
kind create cluster --name rauta-dev --config deploy/kind-config.yaml

# Install Gateway API CRDs
kubectl apply -f https://github.com/kubernetes-sigs/gateway-api/releases/download/v1.1.0/standard-install.yaml

# Create namespaces
kubectl create namespace rauta-system
kubectl create namespace demo
```

### 2. Start Development Loop

```bash
# Continuous development mode (auto-rebuild on change)
skaffold dev

# Terminal output:
# - Builds rauta-control Docker image
# - Deploys to Kind cluster
# - Tails logs from all pods
# - Watches for file changes
# - Auto-rebuilds + redeploys on Rust file changes
```

**That's it!** Edit any Rust file in `control/` or `common/` and Skaffold will:
1. Rebuild the Docker image
2. Redeploy to Kind
3. Show you the logs

---

## Workflows

### Development (Default)

```bash
skaffold dev

# Edit control/src/apis/gateway/http_route.rs
# ... Skaffold detects change ...
# Building rauta-control...
# Deploying...
# Streaming logs...
```

**Port forwards automatically:**
- `localhost:9090` - RAUTA metrics (Prometheus)
- `localhost:8080` - Gateway HTTP endpoint

**Test the gateway:**
```bash
# Send request through gateway
curl -H "Host: echo.local" http://localhost:8080/api
```

### One-off Deploy

```bash
# Build and deploy once (no watch)
skaffold run

# Deploy stays running, but Skaffold exits
# Good for: testing specific commit, CI/CD
```

### Debug Mode

```bash
# Deploy with debug logging
skaffold dev --port-forward

# All port forwards enabled
# Rust backtrace enabled (RUST_BACKTRACE=1)
```

### Delete Everything

```bash
# Stop skaffold dev (Ctrl+C)
# Then delete deployment
skaffold delete
```

---

## Profiles

Skaffold profiles let you customize the workflow.

### Production-like Build

```bash
# Use release build (optimized, slower compile)
skaffold dev -p prod

# Uses Dockerfile.prod
# Cargo profile: release
# No dev tools included
```

### Test Profile

```bash
# Minimal deployment for testing
skaffold run -p test

# Only deploys:
# - GatewayClass
# - RAUTA controller (test config)
# - No demo backends
```

### Observability Stack

```bash
# Deploy with Prometheus + Grafana
RAUTA_OBSERVABILITY=true skaffold dev -p observability

# Access Grafana: kubectl port-forward ...
```

---

## Tips & Tricks

### Faster Rebuilds

Skaffold uses file sync where possible:
```yaml
# Automatically configured in skaffold.yaml
sync:
  manual:
    - src: "control/**/*.rs"
      dest: /app/control
```

For small Rust changes, this can sync files instead of rebuilding entire image.

### Tail Specific Logs

```bash
# Only show RAUTA controller logs
skaffold dev --tail

# Filter by pod
kubectl logs -f -l app=rauta-control -n rauta-system
```

### Skip Tests

```bash
# Skip cargo test phase
skaffold dev --skip-tests
```

### Use Existing Cluster

```bash
# Deploy to existing cluster (not Kind)
skaffold dev --kube-context my-cluster

# Update skaffold.yaml:
deploy:
  kubeContext: my-cluster  # Change from kind-rauta-dev
```

---

## Troubleshooting

### "Image not found in Kind"

```bash
# Skaffold should handle this, but if issues:
skaffold dev --default-repo ""

# Force local build
skaffold config set --global local-cluster true
```

### "Build timeout"

```bash
# Increase build timeout (for slow machines)
skaffold dev --build-timeout 10m
```

### "Deployment not ready"

```bash
# Check pod status
kubectl get pods -n rauta-system

# Check controller logs
kubectl logs -l app=rauta-control -n rauta-system

# Increase status check timeout (in skaffold.yaml)
deploy:
  statusCheckDeadlineSeconds: 600  # 10 minutes
```

### Clean Rebuild

```bash
# Delete Kind cluster and start fresh
kind delete cluster --name rauta-dev
kind create cluster --name rauta-dev --config deploy/kind-config.yaml

# Rebuild from scratch
skaffold dev --cache-artifacts=false
```

---

## Comparison: Before vs After

### Before Skaffold

```bash
# 1. Build
cargo build --release --package control

# 2. Docker build
docker build -f docker/Dockerfile.control-local -t rauta-control:dev .

# 3. Load to Kind
kind load docker-image rauta-control:dev --name rauta-dev

# 4. Deploy
kubectl apply -f deploy/

# 5. Check logs
kubectl logs -f ...

# 6. Make code change
# 7. REPEAT ALL STEPS ❌
```

**Time per iteration:** ~3-5 minutes

### After Skaffold

```bash
# 1. Start
skaffold dev

# 2. Make code change
# 3. Auto-rebuild + redeploy ✅

# Logs streaming automatically
```

**Time per iteration:** ~30-60 seconds (cached builds)

---

## Integration with Seppo

Skaffold is for **development**, Seppo is for **testing**.

**Development workflow (Skaffold):**
```bash
# Terminal 1: Run RAUTA with Skaffold
skaffold dev

# Terminal 2: Manual testing
curl -H "Host: echo.local" http://localhost:8080/api
```

**Integration testing (Seppo):**
```bash
# Automated full test suite
seppo run tests/integration.yaml

# Seppo creates cluster, deploys, tests, cleans up
```

---

## CI/CD Integration

### GitHub Actions

```yaml
# .github/workflows/dev.yml
- name: Deploy to Kind with Skaffold
  run: |
    kind create cluster --name test
    skaffold run -p test

- name: Run integration tests
  run: |
    kubectl wait --for=condition=ready pod -l app=rauta-control
    ./scripts/integration-tests.sh
```

### Local Pre-commit Testing

```bash
# Test before committing
skaffold run -p test
cargo test --workspace
skaffold delete
```

---

## Next Steps

1. **Try it out:** `skaffold dev`
2. **Edit code:** `control/src/proxy/router.rs`
3. **See auto-rebuild:** Watch terminal
4. **Test gateway:** `curl -H "Host: echo.local" http://localhost:8080`
5. **Iterate fast:** Make changes, see results immediately

**Happy hacking! 🚀**

---

False Systems - Building observability tools that actually make sense 🇫🇮
