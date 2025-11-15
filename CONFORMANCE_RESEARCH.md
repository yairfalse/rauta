# Gateway API Conformance Research - Complete Findings

**Date**: November 2025  
**Research Scope**: Gateway API v1.4+ conformance requirements, ReferenceGrant, GAMMA

---

## How to Run Conformance Tests

### Basic Command
```bash
go test ./conformance -args \
  --gateway-class=rauta \
  --conformance-profiles=HTTP
```

### Key Points
- Tests automatically run based on `SupportedFeatures` in GatewayClass status (since Gateway API v1.4)
- No need for `--supported-features`, `--exempt`, or `--all-features` flags anymore
- Implementations MUST populate `supportedFeatures` field before GatewayClass is accepted
- Grace period until v1.5, after which all conformance reports MUST be based on SupportedFeatures

---

## SupportedFeatures Field Specification

### Location
`GatewayClass.status.supportedFeatures`

### Structure (as of v1.4)
```yaml
status:
  supportedFeatures:
    - name: HTTPRoute
    - name: HTTPRouteBackendRequestHeaderModification
    - name: HTTPRouteMethodMatching
    - name: HTTPRouteQueryParamMatching
    - name: HTTPRouteRequestTimeout
    - name: HTTPRouteResponseHeaderModification
```

### Naming Rules
- Start with resource name (e.g., `HTTPRoute`)
- Use PascalCase for the feature portion
- Maximum 64 items
- Each string limited to 128 characters
- Alphanumeric only (letters and numbers)
- Must be sorted in ascending alphabetical order

---

## Feature Mapping: RAUTA Missing Features → Gateway API Features

| RAUTA Missing Feature                 | Gateway API Feature Name                          | Support Level                   |
|---------------------------------------|---------------------------------------------------|---------------------------------|
| 1. HTTPRoute header matching          | Core (built-in)                                   | Core - No explicit feature flag |
| 2. HTTPRoute method matching          | HTTPRouteMethodMatching                           | Extended                        |
| 3. HTTPRoute query parameter matching | HTTPRouteQueryParamMatching                       | Extended                        |
| 4. RequestHeaderModifier filter       | HTTPRouteBackendRequestHeaderModification         | Extended                        |
| 5. ResponseHeaderModifier filter      | HTTPRouteResponseHeaderModification               | Extended                        |
| 6. RequestRedirect filter             | Core (built-in)                                   | Core - No explicit feature flag |
| 7. Timeout support                    | HTTPRouteRequestTimeout + HTTPRouteBackendTimeout | Extended                        |

**Important Discovery**: Header matching and RequestRedirect are Core features, not Extended. They don't require a separate feature flag - just implementing HTTPRoute implies support for them.

---

## Implementation Timeline

| Feature                   | Complexity | Estimated Time       |
|---------------------------|------------|----------------------|
| 1. Header Matching        | Medium     | 4-6 hours            |
| 2. Method Matching        | Low        | 2-3 hours            |
| 3. Query Param Matching   | Medium     | 4-5 hours            |
| 4. RequestHeaderModifier  | Medium     | 5-7 hours            |
| 5. ResponseHeaderModifier | Low        | 2-3 hours (reuse #4) |
| 6. RequestRedirect        | Medium     | 5-7 hours            |
| 7. Timeout Support        | Medium     | 4-6 hours            |
| **Total**                 |            | **26-37 hours**      |

### Recommended Order
1. Header Matching (foundational for filters)
2. Method Matching (easiest win)
3. Query Param Matching (completes matching layer)
4. RequestHeaderModifier (filters foundation)
5. ResponseHeaderModifier (builds on #4)
6. RequestRedirect (uses matching + filters)
7. Timeout Support (cross-cutting concern)

### Sprint Plan (1-2 weeks)
- **Week 1**: Features 1-4 (matching + request filters)
- **Week 2**: Features 5-7 (response filters + timeouts)

---

## Files to Modify

- `control/src/routing/matcher.rs` - Matching logic
- `control/src/routing/filters.rs` - Filter implementations (**new file**)
- `control/src/routing/route.rs` - Data structures
- `control/src/proxy/server.rs` - Integration
- `control/src/controller/httproute.rs` - K8s parsing
- `control/src/controller/gatewayclass.rs` - Status updates

---

## Final GatewayClass Status Update

After implementing all 7 features:

```rust
// control/src/controller/gatewayclass.rs
fn build_gatewayclass_status() -> GatewayClassStatus {
    GatewayClassStatus {
        conditions: vec![/* accepted condition */],
        supported_features: Some(vec![
            // Core features (implied by HTTPRoute)
            SupportedFeature {
                name: "HTTPRoute".to_string(),
            },
            // Extended features (explicit, alphabetically sorted)
            SupportedFeature {
                name: "HTTPRouteBackendRequestHeaderModification".to_string(),
            },
            SupportedFeature {
                name: "HTTPRouteBackendTimeout".to_string(),
            },
            SupportedFeature {
                name: "HTTPRouteMethodMatching".to_string(),
            },
            SupportedFeature {
                name: "HTTPRouteQueryParamMatching".to_string(),
            },
            SupportedFeature {
                name: "HTTPRouteRequestTimeout".to_string(),
            },
            SupportedFeature {
                name: "HTTPRouteResponseHeaderModification".to_string(),
            },
        ]),
    }
}
```

---

## Verification Checklist

After each feature:
- [ ] Unit tests pass (`cargo test`)
- [ ] Integration test passes (`go test ./conformance`)
- [ ] Code formatted (`cargo fmt`)
- [ ] Code linted (`cargo clippy`)
- [ ] GatewayClass status updated (if Extended feature)
- [ ] Metrics added
- [ ] Documentation updated
- [ ] Commit message follows convention

---

## Integration Tests (Conformance Suite)

```bash
# After each feature implementation, run conformance tests
cd /Users/yair/projects/rauta
go test ./conformance -args \
  --gateway-class=rauta \
  --conformance-profiles=HTTP \
  -v

# Expected results after all features:
# ✅ HTTPRouteHeaderMatching: PASS
# ✅ HTTPRouteMethodMatching: PASS
# ✅ HTTPRouteQueryParamMatching: PASS
# ✅ HTTPRouteBackendRequestHeaderModification: PASS
# ✅ HTTPRouteResponseHeaderModification: PASS
# ✅ RequestRedirect: PASS
# ✅ HTTPRouteRequestTimeout: PASS
# ✅ HTTPRouteBackendTimeout: PASS
```

---

## Design Principles

- **TDD**: RED → GREEN → REFACTOR for each feature
- **Small commits**: Maximum 30-50 lines per commit
- **Integration testing**: Use Gateway API conformance tests as integration tests
- **Incremental**: Implement one feature at a time, verify with conformance tests

---

**See CONFORMANCE_IMPLEMENTATION.md for detailed code examples and TDD approach for each feature.**
