# ADR 002: Active Health Checking with Observability-First Design

**Status**: Accepted
**Date**: 2025-11-13
**Authors**: Jane Doe
**Related**: [ADR 001](001-hostnetwork-daemonset-architecture.md)

## Context

RAUTA needs active health checking to detect failed backends proactively (before user requests fail). However, health checking is a double-edged sword:
- **Too aggressive**: False positives mark healthy backends unhealthy
- **Too passive**: Slow to detect real failures
- **No observability**: Operators can't tune thresholds or debug issues

This ADR documents our **observability-first** approach to active health checking.

## Decision

We implement active health checking with three core principles:

### 1. Easy to Observe

**Prometheus Metrics** (following Cortex/Mimir pattern):
```
# Backend health status (1=healthy, 0=unhealthy)
rauta_backend_health_status{backend="10.0.1.1:8080", route="/api"} 1

# Total probes sent
rauta_health_check_probes_total{backend="10.0.1.1:8080", result="success"} 1234
rauta_health_check_probes_total{backend="10.0.1.1:8080", result="failure"} 5
rauta_health_check_probes_total{backend="10.0.1.1:8080", result="timeout"} 2

# Probe duration (histogram for P50/P99/P999)
rauta_health_check_duration_seconds_bucket{backend="10.0.1.1:8080", result="success", le="0.001"} 100
rauta_health_check_duration_seconds_bucket{backend="10.0.1.1:8080", result="success", le="0.010"} 950
rauta_health_check_duration_seconds_sum{backend="10.0.1.1:8080", result="success"} 4.5
rauta_health_check_duration_seconds_count{backend="10.0.1.1:8080", result="success"} 1000

# Consecutive failures (gauge for alerting)
rauta_health_check_consecutive_failures{backend="10.0.1.1:8080"} 2

# State transitions (detect flapping)
rauta_health_state_transitions_total{backend="10.0.1.1:8080", from="healthy", to="unhealthy"} 5
rauta_health_state_transitions_total{backend="10.0.1.1:8080", from="unhealthy", to="healthy"} 4
```

**Structured Logging** (tracing):
```rust
info!(
    backend = "10.0.1.1:8080",
    route = "/api",
    "Starting health checks"
);

warn!(
    backend = "10.0.1.1:8080",
    error = "connection refused",
    consecutive_failures = 2,
    "Health check failed (connection error)"
);

info!(
    backend = "10.0.1.1:8080",
    old_status = "unhealthy",
    new_status = "healthy",
    "Health status transitioned"
);
```

### 2. Easy to Configure

**YAML Configuration** (`config.yaml`):
```yaml
health_check:
  enabled: false  # Disabled by default for safety
  interval_secs: 5  # Probe every 5 seconds
  timeout_secs: 2  # 2 second TCP timeout
  unhealthy_threshold: 3  # 3 failures → unhealthy
  healthy_threshold: 2  # 2 successes → healthy
```

**Environment Variables** (for Kubernetes):
```bash
RAUTA_HEALTH_CHECK_ENABLED=true
RAUTA_HEALTH_CHECK_INTERVAL_SECS=10
RAUTA_HEALTH_CHECK_TIMEOUT_SECS=3
RAUTA_HEALTH_CHECK_UNHEALTHY_THRESHOLD=5
RAUTA_HEALTH_CHECK_HEALTHY_THRESHOLD=3
```

**Why Disabled by Default?**
- Prevents breaking existing deployments
- Operators must explicitly opt-in
- Allows testing in staging before production

### 3. Easy to Maintain

**Clean Shutdown** (no task leaks):
```rust
// Graceful shutdown with CancellationToken
health_checker.shutdown();  // Cancels all background tasks
```

**Registry Injection** (testable, no globals):
```rust
// BAD: Global metrics (Cortex anti-pattern)
lazy_static! {
    static ref HEALTH_METRICS: IntGaugeVec = ...;
}

// GOOD: Injected registry (Cortex/Mimir pattern)
let checker = HealthChecker::new(config, &registry);
```

**Dual Health Checking** (passive + active):
```rust
// Skip backend if EITHER check fails
let is_healthy_passive = circuit_breaker.is_healthy(&backend);  // Real traffic
let is_healthy_active = health_checker.is_healthy(&backend);    // TCP probes

if !is_healthy_passive || !is_healthy_active {
    continue;  // Try next backend
}
```

## Implementation Details

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ Router (Backend Selection)                                  │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ 1. Maglev Hash (consistent hashing)                    │ │
│  │    backend_index = maglev_lookup(flow_hash)            │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ 2. Passive Health Check (Circuit Breaker)              │ │
│  │    if circuit_breaker.is_unhealthy(backend):           │ │
│  │        skip backend, try next                           │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ 3. Active Health Check (TCP Probes)                    │ │
│  │    if health_checker.is_unhealthy(backend):            │ │
│  │        skip backend, try next                           │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                              │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ 4. Connection Draining (Graceful Removal)              │ │
│  │    if backend.is_draining():                            │ │
│  │        skip backend, try next                           │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ HealthChecker (Background Tasks)                            │
│                                                              │
│  For each backend:                                           │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ loop {                                                  │ │
│  │     tokio::select! {                                    │ │
│  │         _ = cancel_token.cancelled() => break;          │ │
│  │         _ = interval.tick() => {                        │ │
│  │             let result = TcpStream::connect(backend)    │ │
│  │                 .timeout(2s)                             │ │
│  │                 .await;                                  │ │
│  │             update_state(result);                        │ │
│  │             record_metrics(result);                      │ │
│  │         }                                                │ │
│  │     }                                                    │ │
│  │ }                                                        │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

### State Machine

```
        ┌─────────────────────────────────────┐
        │                                     │
        │         HEALTHY                     │
        │  (consecutive_successes >= 2)       │
        │                                     │
        └──────────┬────────────────┬─────────┘
                   │                │
        success    │                │  3 consecutive failures
                   │                │
                   │                ▼
                   │         ┌─────────────────┐
                   │         │                 │
                   │         │   UNHEALTHY     │
                   │         │                 │
                   │         └─────────┬───────┘
                   │                   │
                   │                   │  2 consecutive successes
                   │                   │
                   └───────────────────┘
```

### Probe Types

**Current**: TCP Connect (fast, simple)
```rust
TcpStream::connect(backend).timeout(2s).await
```

**Future**: HTTP GET (application-aware)
```rust
http_client.get(format!("http://{backend}/health")).timeout(2s).await
```

## Operational Guide

### Enabling Health Checks

**Development/Staging** (aggressive, fast failover):
```yaml
health_check:
  enabled: true
  interval_secs: 3  # Check every 3s
  timeout_secs: 1  # Fast timeout
  unhealthy_threshold: 2  # Fail fast (2 failures)
  healthy_threshold: 1  # Recover fast (1 success)
```

**Production** (conservative, avoid false positives):
```yaml
health_check:
  enabled: true
  interval_secs: 10  # Check every 10s (less aggressive)
  timeout_secs: 3  # More lenient timeout
  unhealthy_threshold: 5  # More failures required
  healthy_threshold: 3  # More successes required
```

### Monitoring Queries

**Unhealthy Backends**:
```promql
# Alert if any backend is unhealthy for >1 minute
rauta_backend_health_status{} == 0

# Count unhealthy backends per route
sum by (route) (rauta_backend_health_status{} == 0)
```

**Flapping Backends** (unstable, need investigation):
```promql
# Alert if backend flaps >10 times in 5 minutes
rate(rauta_health_state_transitions_total[5m]) > 10
```

**Slow Health Checks** (network issues):
```promql
# P99 health check duration
histogram_quantile(0.99,
  rate(rauta_health_check_duration_seconds_bucket[5m])
)
```

**Failed Probes**:
```promql
# Failure rate per backend
rate(rauta_health_check_probes_total{result="failure"}[5m])
  /
rate(rauta_health_check_probes_total{}[5m])
```

### Tuning Guidelines

**If you see false positives** (healthy backends marked unhealthy):
1. Increase `timeout_secs` (2s → 3s → 5s)
2. Increase `unhealthy_threshold` (3 → 5 → 7)
3. Increase `interval_secs` (5s → 10s) to reduce load

**If you see slow failover** (unhealthy backends not detected fast enough):
1. Decrease `interval_secs` (5s → 3s → 1s)
2. Decrease `unhealthy_threshold` (3 → 2)
3. Decrease `timeout_secs` (2s → 1s) - CAREFUL!

**If you see flapping**:
1. Increase BOTH thresholds (unhealthy=5, healthy=3)
2. This creates "hysteresis" to avoid rapid state changes

### Debugging

**Check current health status**:
```bash
# Query Prometheus
curl http://localhost:9090/api/v1/query?query=rauta_backend_health_status

# Expected output:
{
  "status": "success",
  "data": {
    "resultType": "vector",
    "result": [
      {
        "metric": {
          "backend": "10.0.1.1:8080",
          "route": "/api"
        },
        "value": [1731456789, "1"]  # 1 = healthy
      }
    ]
  }
}
```

**Check logs**:
```bash
kubectl logs -n rauta-system rauta-xxxxxx | grep "Health check"

# Expected output:
INFO Starting health checks backend=10.0.1.1:8080 route=/api
DEBUG Health check succeeded backend=10.0.1.1:8080 latency_ms=5 consecutive_successes=1
WARN Health check failed (connection error) backend=10.0.1.2:8080 error="connection refused" consecutive_failures=2
INFO Health status transitioned backend=10.0.1.2:8080 old_status=healthy new_status=unhealthy
```

## Consequences

### Positive

✅ **Proactive failure detection** - Catch backend failures before user requests
✅ **Observability-first** - Rich metrics + structured logs
✅ **Configurable** - Easy to tune per environment
✅ **Testable** - Registry injection, no globals
✅ **Clean shutdown** - CancellationToken prevents task leaks
✅ **Safe defaults** - Disabled by default, opt-in

### Negative

❌ **Additional load** - TCP probes every 5s per backend
❌ **Complexity** - More configuration to tune
❌ **False positives** - Network blips can mark backends unhealthy

### Mitigations

- **Load**: TCP connect is very lightweight (<1ms)
- **Complexity**: Good defaults + operational docs
- **False positives**: Tunable thresholds + hysteresis

## References

- **Envoy Health Checking**: https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/upstream/health_checking
- **NGINX Health Checks**: https://docs.nginx.com/nginx/admin-guide/load-balancer/http-health-check/
- **Prometheus Best Practices**: https://prometheus.io/docs/practices/naming/
- **Cortex/Mimir Registry Injection**: https://github.com/grafana/mimir/blob/main/docs/sources/mimir/references/architecture/components/distributor.md

## Future Enhancements

1. **HTTP Health Checks** - GET `/health` endpoint
2. **gRPC Health Checks** - Standard gRPC health protocol
3. **Connection Pooling** - Reuse TCP connections for probes
4. **Per-Route Configuration** - Different thresholds per route
5. **Adaptive Thresholds** - Auto-tune based on historical data

---

**Decision Made**: 2025-11-13
**Status**: Implemented in TDD REFACTOR phase
**Tests**: 103 passing ✅
