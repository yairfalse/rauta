# RAUTA Production Readiness Checklist

**Current Status**: Stage 1 Complete (Basic Gateway API controller) | Production-Ready: **NOT YET**

This document tracks the critical requirements for RAUTA to be production-ready as a Gateway API controller with WASM plugin extensibility.

---

## Critical Production Requirements

### 1. Gateway API Conformance Testing ❌ **BLOCKING**

**Status**: Not started

**Why Critical**: The official Gateway API conformance test suite is THE industry standard for proving production-readiness. Without passing these tests, RAUTA cannot claim to be a compliant Gateway API implementation.

**What's Required**:

```bash
# Install conformance test suite
go install sigs.k8s.io/gateway-api/conformance/cmd/suite@latest

# Run tests against RAUTA
kubectl apply -f deploy/rauta-gateway-class.yaml
suite --gateway-class rauta --output conformance-report.yaml

# Must pass ALL Core tests for Gateway + HTTPRoute profile
```

**Conformance Levels**:
- **CORE** (Gateway + HTTPRoute): **REQUIRED** for production
  - HTTPRoute path matching (exact, prefix, regex)
  - HTTPRoute header matching
  - HTTPRoute method matching
  - HTTPRoute query parameter matching
  - HTTPRoute filters (RequestHeaderModifier, ResponseHeaderModifier, RequestRedirect)
  - HTTPRoute backend references
  - HTTPRoute timeout annotations
  - Gateway status conditions
  - GatewayClass acceptance

- **EXTENDED** (Optional but valuable):
  - HTTPRoute request mirroring
  - HTTPRoute URL rewrite
  - HTTPRoute backend weights (✅ **IMPLEMENTED** - Maglev with largest-remainder method)
  - TLS termination (🚧 implemented, needs conformance validation)
  - Cross-namespace references via ReferenceGrant (❌ **MISSING** - see below)

**References**:
- Official conformance docs: https://gateway-api.sigs.k8s.io/concepts/conformance/
- GEP-917 (Conformance Testing): https://gateway-api.sigs.k8s.io/geps/gep-917/
- Test source: https://github.com/kubernetes-sigs/gateway-api/tree/main/conformance

**Action Items**:
- [ ] Set up conformance test environment (kind cluster)
- [ ] Run conformance suite and capture initial failure report
- [ ] Implement missing Core features (see failures)
- [ ] Re-run until 100% Core conformance
- [ ] Add conformance badge to README
- [ ] Publish official conformance report

**Implementation Example** (what we need):
```rust
// control/src/apis/gateway/http_route.rs
async fn reconcile_http_route(route: Arc<HTTPRoute>, ctx: Arc<Context>) -> Result<Action> {
    // ✅ HAVE: Path matching (prefix, exact)
    // ✅ HAVE: Backend selection with weights
    // ❌ MISSING: Header matching
    // ❌ MISSING: Method matching (currently hardcoded to all methods)
    // ❌ MISSING: Query parameter matching
    // ❌ MISSING: RequestHeaderModifier filter
    // ❌ MISSING: ResponseHeaderModifier filter
    // ❌ MISSING: RequestRedirect filter
    // ❌ MISSING: Timeout support

    for rule in &route.spec.rules {
        for match_rule in &rule.matches {
            // NEED: Implement all match types
            if let Some(headers) = &match_rule.headers {
                // Match headers (not implemented!)
            }
            if let Some(method) = &match_rule.method {
                // Match HTTP method (not implemented!)
            }
            if let Some(query_params) = &match_rule.query_params {
                // Match query parameters (not implemented!)
            }
        }

        for filter in &rule.filters {
            // NEED: Implement all filter types
            match filter.type_ {
                FilterType::RequestHeaderModifier => {
                    // Modify request headers (not implemented!)
                }
                FilterType::ResponseHeaderModifier => {
                    // Modify response headers (not implemented!)
                }
                FilterType::RequestRedirect => {
                    // HTTP redirect (not implemented!)
                }
                _ => {}
            }
        }
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}
```

---

### 2. ReferenceGrant Support ❌ **BLOCKING**

**Status**: Not implemented

**Why Critical**: Security mechanism required for cross-namespace Service references. Without this, RAUTA is vulnerable to confused deputy attacks (CVE-2021-25740) and cannot safely support multi-tenant clusters.

**What's Required**:

**ReferenceGrant CRD** (part of Gateway API standard):
```yaml
apiVersion: gateway.networking.k8s.io/v1beta1
kind: ReferenceGrant
metadata:
  name: allow-routes-to-backend-services
  namespace: backend-ns  # Where the Service lives
spec:
  from:
  - group: gateway.networking.k8s.io
    kind: HTTPRoute
    namespace: route-ns  # Where the HTTPRoute lives
  to:
  - group: ""
    kind: Service
```

**Security Model**:
- HTTPRoute in namespace A wants to reference Service in namespace B
- Without ReferenceGrant: **DENY** (default secure)
- With ReferenceGrant created by namespace B owner: **ALLOW**
- Prevents namespace A from accessing namespace B resources without explicit permission

**Current Gap**:
```rust
// control/src/apis/gateway/http_route.rs (CURRENT - INSECURE!)
async fn resolve_service(
    &self,
    backend_ref: &BackendObjectReference,
    route_namespace: &str,
) -> Result<Vec<Backend>> {
    let service_namespace = backend_ref.namespace.as_deref()
        .unwrap_or(route_namespace);  // Cross-namespace reference!

    // ❌ MISSING: Check for ReferenceGrant before allowing access
    // Currently allows ANY HTTPRoute to reference ANY Service in ANY namespace
    // This is a SECURITY VULNERABILITY

    let service = self.client
        .get::<Service>(&backend_ref.name, service_namespace)
        .await?;  // Should fail if no ReferenceGrant exists!

    Ok(resolve_backends(service))
}
```

**Required Implementation**:
```rust
// control/src/apis/gateway/reference_grant.rs (NEW FILE NEEDED)
use gateway_api::apis::v1beta1::reference_grants::ReferenceGrant;

pub struct ReferenceGrantChecker {
    client: Client,
    grants_cache: Arc<RwLock<HashMap<NamespaceKey, Vec<ReferenceGrant>>>>,
}

impl ReferenceGrantChecker {
    /// Check if a cross-namespace reference is allowed by a ReferenceGrant
    pub async fn is_reference_allowed(
        &self,
        from_namespace: &str,
        from_group: &str,
        from_kind: &str,
        to_namespace: &str,
        to_group: &str,
        to_kind: &str,
    ) -> bool {
        // Same namespace? Always allowed
        if from_namespace == to_namespace {
            return true;
        }

        // Cross-namespace: check for ReferenceGrant in target namespace
        let grants = self.grants_cache.read().unwrap();
        let key = (to_namespace.to_string(), to_group.to_string(), to_kind.to_string());

        grants.get(&key).map(|grants| {
            grants.iter().any(|grant| {
                grant.spec.from.iter().any(|from| {
                    from.group == from_group
                        && from.kind == from_kind
                        && from.namespace == from_namespace
                }) && grant.spec.to.iter().any(|to| {
                    to.group == to_group && to.kind == to_kind
                })
            })
        }).unwrap_or(false)
    }
}

// control/src/apis/gateway/http_route.rs (FIXED)
async fn resolve_service(
    &self,
    backend_ref: &BackendObjectReference,
    route_namespace: &str,
) -> Result<Vec<Backend>> {
    let service_namespace = backend_ref.namespace.as_deref()
        .unwrap_or(route_namespace);

    // ✅ SECURITY: Check ReferenceGrant for cross-namespace access
    if service_namespace != route_namespace {
        let allowed = self.reference_grant_checker.is_reference_allowed(
            route_namespace,
            "gateway.networking.k8s.io",
            "HTTPRoute",
            service_namespace,
            "",  // Core API group
            "Service",
        ).await;

        if !allowed {
            warn!(
                "HTTPRoute {}/{} cannot reference Service {}/{}: No ReferenceGrant found",
                route_namespace, route_name, service_namespace, backend_ref.name
            );
            return Err(Error::ReferenceNotPermitted);
        }
    }

    let service = self.client
        .get::<Service>(&backend_ref.name, service_namespace)
        .await?;

    Ok(resolve_backends(service))
}
```

**Watcher Required**:
```rust
// control/src/apis/gateway/reference_grant_watcher.rs (NEW FILE NEEDED)
pub async fn watch_reference_grants(
    client: Client,
    checker: Arc<ReferenceGrantChecker>,
) -> Result<()> {
    let api: Api<ReferenceGrant> = Api::all(client);
    let watcher = watcher(api, Config::default());

    tokio::pin!(watcher);
    while let Some(event) = watcher.try_next().await? {
        match event {
            Event::Applied(grant) => {
                checker.add_grant(grant).await;
            }
            Event::Deleted(grant) => {
                checker.remove_grant(grant).await;
                // IMPORTANT: Revoke access immediately when grant is removed!
            }
            _ => {}
        }
    }

    Ok(())
}
```

**Action Items**:
- [ ] Add `gateway-api` crate dependency with ReferenceGrant support
- [ ] Implement `ReferenceGrantChecker` with caching
- [ ] Add ReferenceGrant watcher (kube-runtime)
- [ ] Modify `http_route.rs` to check grants before cross-namespace references
- [ ] Add unit tests for grant checking logic
- [ ] Add integration test with cross-namespace HTTPRoute + ReferenceGrant
- [ ] Document security model in README

**References**:
- ReferenceGrant API: https://gateway-api.sigs.k8s.io/api-types/referencegrant/
- GEP-709 (Cross Namespace References): https://gateway-api.sigs.k8s.io/geps/gep-709/
- Security Model: https://gateway-api.sigs.k8s.io/concepts/security-model/
- CVE-2021-25740 (why this matters): Confused deputy attack via cross-namespace references

---

### 3. GAMMA Support (Gateway API for Mesh) ⚠️ **OPTIONAL** (But Future-Proof)

**Status**: Not implemented (not blocking, but strategic)

**Why Important**: GAMMA (Gateway API for Mesh Management and Administration) extends Gateway API to support **east-west traffic** (service-to-service within the cluster). While RAUTA currently focuses on **north-south** (ingress), GAMMA support enables:
- Service mesh use cases (without eBPF complexity)
- HTTPRoute attached to Service (not Gateway) for east-west routing
- Consistent API for both ingress and internal traffic
- Future-proofing for service mesh features (Stage 2+ in roadmap)

**What's Required**:

**GAMMA Pattern** (HTTPRoute → Service instead of HTTPRoute → Gateway):
```yaml
# Traditional north-south (ingress)
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: ingress-route
spec:
  parentRefs:
  - name: rauta-gateway
    kind: Gateway  # ← Routes attached to Gateway
  rules:
  - backendRefs:
    - name: backend-service
      port: 8080

---
# GAMMA east-west (service mesh)
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: service-route
spec:
  parentRefs:
  - name: frontend-service
    kind: Service  # ← Routes attached to Service!
    group: ""
  rules:
  - matches:
    - path:
        value: /v2
    backendRefs:
    - name: backend-v2  # Canary routing for service-to-service calls
      port: 8080
      weight: 10
    - name: backend-v1
      port: 8080
      weight: 90
```

**Use Case**:
```
Without GAMMA:
  frontend-pod → backend-service (round-robin, no routing logic)

With GAMMA:
  frontend-pod → backend-service (intercepted by RAUTA)
    → HTTPRoute rules apply (canary, header-based routing, etc.)
    → backend-v1 (90%) or backend-v2 (10%)
```

**Implementation Gap**:
```rust
// control/src/apis/gateway/http_route.rs (CURRENT)
async fn reconcile_http_route(route: Arc<HTTPRoute>, ctx: Arc<Context>) -> Result<Action> {
    for parent_ref in &route.spec.parent_refs {
        match parent_ref.kind.as_str() {
            "Gateway" => {
                // ✅ IMPLEMENTED: Attach to Gateway (ingress)
                apply_route_to_gateway(route, parent_ref, ctx).await?;
            }
            "Service" => {
                // ❌ NOT IMPLEMENTED: GAMMA support
                // Need to:
                // 1. Watch Service as parent
                // 2. Intercept traffic TO this Service
                // 3. Apply HTTPRoute rules for east-west routing
                warn!("GAMMA (Service parentRef) not supported yet");
                return Ok(Action::requeue(Duration::from_secs(300)));
            }
            _ => {
                return Err(Error::UnsupportedParentRef);
            }
        }
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}
```

**Decision**: Defer GAMMA to Stage 2 (after WASM plugins). Focus on north-south ingress first.

**Action Items** (Future):
- [ ] Research GAMMA specification (GEP-1324)
- [ ] Design east-west traffic interception strategy (eBPF sockops or userspace proxy)
- [ ] Implement Service parentRef support in HTTPRoute reconciler
- [ ] Add GAMMA conformance tests
- [ ] Document service mesh capabilities

**References**:
- GAMMA Initiative: https://gateway-api.sigs.k8s.io/mesh/gamma/
- GEP-1324 (Service Mesh): https://gateway-api.sigs.k8s.io/geps/gep-1324/
- Mesh Conformance: https://gateway-api.sigs.k8s.io/geps/gep-1686/

---

## Production Readiness Summary

### Blocking (Must Have Before v1.0)

| Requirement | Status | Effort | Priority |
|-------------|--------|--------|----------|
| **Gateway API Conformance** | ❌ Not Started | 2-4 weeks | **P0** |
| **ReferenceGrant Support** | ❌ Not Started | 1 week | **P0** |
| **HTTPRoute Header Matching** | ❌ Missing | 3 days | **P0** |
| **HTTPRoute Method Matching** | ❌ Missing | 2 days | **P0** |
| **HTTPRoute Query Matching** | ❌ Missing | 3 days | **P0** |
| **RequestHeaderModifier Filter** | ❌ Missing | 4 days | **P0** |
| **ResponseHeaderModifier Filter** | ❌ Missing | 4 days | **P0** |
| **RequestRedirect Filter** | ❌ Missing | 3 days | **P0** |
| **HTTPRoute Timeouts** | ❌ Missing | 2 days | **P0** |

### Important (Should Have for Production)

| Requirement | Status | Effort | Priority |
|-------------|--------|--------|----------|
| **TLS Certificate Rotation** | 🚧 Partial | 2 days | **P1** |
| **Graceful Shutdown** | ✅ Done | - | ✅ |
| **Connection Draining** | ✅ Done | - | ✅ |
| **Health Checks** | 🚧 Passive Only | 3 days | **P1** |
| **Observability (Metrics)** | ✅ Done | - | ✅ |
| **Observability (Tracing)** | ❌ Missing | 1 week | **P1** |

### Nice to Have (Future)

| Requirement | Status | Effort | Priority |
|-------------|--------|--------|----------|
| **GAMMA (Service Mesh)** | ❌ Not Planned | 4-6 weeks | **P2** |
| **HTTPRoute Request Mirroring** | ❌ Missing | 1 week | **P2** |
| **HTTPRoute URL Rewrite** | ❌ Missing | 1 week | **P2** |
| **Active Health Checks** | ❌ Missing | 1 week | **P2** |
| **Rate Limiting** | ❌ Missing | 2 weeks | **P2** |

---

## Recommended Roadmap

### Phase 1: Conformance (4-6 weeks)

**Goal**: Pass 100% Gateway API Core conformance tests

**Week 1-2: Missing Match Types**
- Implement HTTPRoute header matching
- Implement HTTPRoute method matching
- Implement HTTPRoute query parameter matching
- Add unit tests for all match types
- Validate against conformance suite

**Week 3-4: HTTPRoute Filters**
- Implement RequestHeaderModifier filter
- Implement ResponseHeaderModifier filter
- Implement RequestRedirect filter
- Add filter combination tests
- Validate against conformance suite

**Week 5: ReferenceGrant Security**
- Implement ReferenceGrant watcher
- Implement cross-namespace permission checking
- Add security tests (deny without grant, allow with grant)
- Document security model

**Week 6: Final Validation**
- Run full conformance suite
- Fix any remaining failures
- Publish conformance report
- Update README with conformance badge

**Deliverable**: Official Gateway API conformance report showing 100% Core support

### Phase 2: Production Hardening (2-3 weeks)

**Goal**: Production-ready stability and observability

**Week 7: Observability**
- Implement OTLP tracing (spans for request lifecycle)
- Add distributed tracing headers (traceparent, tracestate)
- Integrate with Jaeger/Tempo
- Add trace sampling configuration

**Week 8: Reliability**
- Implement active health checks (HTTP probe endpoints)
- Add circuit breaker logic (fail fast on unhealthy backends)
- Improve TLS certificate hot-reload
- Add retry logic for transient failures

**Week 9: Testing & Documentation**
- Add chaos testing (kill backends mid-request, network delays)
- Performance testing (load test with 10K rps)
- Write production deployment guide
- Create troubleshooting runbook

**Deliverable**: Production deployment guide with SLOs (99.9% uptime, p99 < 50ms)

### Phase 3: WASM Plugins (4-6 weeks) - **THE DIFFERENTIATOR**

**Goal**: Safe extensibility via WASM (see CLAUDE.md for details)

**This is what makes RAUTA unique** - see CLAUDE.md roadmap.

---

## How to Run Conformance Tests

```bash
# 1. Install conformance test suite
go install sigs.k8s.io/gateway-api/conformance/cmd/suite@latest

# 2. Deploy RAUTA to kind cluster
./deploy/deploy-to-kind.sh

# 3. Run conformance tests
suite \
  --gateway-class rauta \
  --supported-features Gateway,HTTPRoute \
  --output conformance-report.yaml

# 4. View results
cat conformance-report.yaml

# Expected (current state):
# ❌ HTTPRouteHeaderMatching: FAIL
# ❌ HTTPRouteMethodMatching: FAIL
# ❌ HTTPRouteQueryParamMatching: FAIL
# ❌ HTTPRouteRequestHeaderModifier: FAIL
# ❌ HTTPRouteResponseHeaderModifier: FAIL
# ❌ HTTPRouteRequestRedirect: FAIL
# ❌ HTTPRouteReferenceGrant: FAIL
# ✅ HTTPRoutePathMatch: PASS
# ✅ HTTPRouteBackendWeights: PASS (Maglev + largest-remainder!)
```

---

## Definition of Production-Ready

RAUTA is **production-ready** when:

- [ ] ✅ Passes 100% Gateway API Core conformance tests
- [ ] ✅ ReferenceGrant security implemented and tested
- [ ] ✅ All Core HTTPRoute features working (match types, filters)
- [ ] ✅ TLS termination with hot-reload
- [ ] ✅ Graceful shutdown with connection draining
- [ ] ✅ Observability (metrics + traces)
- [ ] ✅ Health checks (passive + active)
- [ ] ✅ Load tested at 50K+ rps with <50ms p99 latency
- [ ] ✅ Chaos tested (backend failures, network issues)
- [ ] ✅ Production deployment documentation
- [ ] ✅ Troubleshooting runbook

**Current Status**: 4/11 complete (~36%)

**ETA to Production-Ready**: 8-10 weeks (assuming full-time work)

---

## Questions to Resolve

**Q1: Should we support GAMMA (Service Mesh) in v1.0?**
- **Recommendation**: No. Focus on north-south ingress first. GAMMA is future work.
- **Reasoning**: Adds significant complexity (east-west traffic interception). WASM plugins are higher priority for differentiation.

**Q2: Which Extended features should we prioritize?**
- **Recommendation**: Request mirroring (for blue/green testing), URL rewrite (for API versioning).
- **Reasoning**: High user value, relatively simple to implement in userspace proxy.

**Q3: Should we support TLSRoute, TCPRoute, UDPRoute?**
- **Recommendation**: Start with HTTPRoute only. Add TLSRoute in v1.1 if users request it.
- **Reasoning**: HTTPRoute covers 90% of use cases. Additional route types can be added incrementally.

---

## References

**Official Gateway API Resources**:
- Conformance Docs: https://gateway-api.sigs.k8s.io/concepts/conformance/
- ReferenceGrant: https://gateway-api.sigs.k8s.io/api-types/referencegrant/
- GAMMA (Mesh): https://gateway-api.sigs.k8s.io/mesh/gamma/
- Implementations List: https://gateway-api.sigs.k8s.io/implementations/

**Reference Implementations**:
- Envoy Gateway: https://github.com/envoyproxy/gateway
- Kong Gateway: https://github.com/Kong/kubernetes-ingress-controller
- Istio: https://github.com/istio/istio

**RAUTA-Specific**:
- Architecture: CLAUDE.md
- Current Implementation: control/src/apis/gateway/
- Performance Benchmarks: docs/PERFORMANCE_COMPARISON.md

---

**Next Step**: Start Phase 1 (Conformance) by implementing HTTPRoute header matching.

**Timeline**: 8-10 weeks to production-ready v1.0
