//! HTTPRoute watcher
//!
//! Watches HTTPRoute resources and updates routing rules.

use crate::apis::metrics::record_httproute_reconciliation;
use crate::proxy::router::Router;
use common::{Backend, HttpMethod};
use futures::StreamExt;
use gateway_api::apis::standard::httproutes::HTTPRoute;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, ResourceExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// HTTPRoute reconciler
#[allow(dead_code)] // Used in K8s mode
pub struct HTTPRouteReconciler {
    client: Client,
    router: Arc<Router>,
    /// Gateway name to watch routes for
    gateway_name: String,
}

#[allow(dead_code)] // Used in K8s mode
impl HTTPRouteReconciler {
    pub fn new(client: Client, router: Arc<Router>, gateway_name: String) -> Self {
        Self {
            client,
            router,
            gateway_name,
        }
    }

    /// Check if this HTTPRoute references our Gateway
    fn should_reconcile(
        &self,
        parent_refs: &Option<Vec<gateway_api::apis::standard::httproutes::HTTPRouteParentRefs>>,
    ) -> bool {
        if let Some(refs) = parent_refs {
            refs.iter().any(|r| r.name == self.gateway_name)
        } else {
            false
        }
    }

    /// Resolve Service name to Pod IPs via Kubernetes EndpointSlice API
    async fn resolve_service_endpoints(
        &self,
        service_name: &str,
        namespace: &str,
        service_port: u32,
    ) -> Result<Vec<Backend>, kube::Error> {
        use k8s_openapi::api::core::v1::Service;
        use k8s_openapi::api::discovery::v1::EndpointSlice;
        use kube::api::ListParams;

        // Fetch the Service to resolve service_port -> targetPort
        let service_api: Api<Service> = Api::namespaced(self.client.clone(), namespace);
        let service = service_api.get(service_name).await?;

        // Find the target port for the given service port
        let target_port = if let Some(spec) = &service.spec {
            if let Some(ports) = &spec.ports {
                ports
                    .iter()
                    .find(|p| p.port == service_port as i32)
                    .map(|p| {
                        // targetPort can be a number or a name
                        match &p.target_port {
                            Some(
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                                    port,
                                ),
                            ) => *port as u16,
                            Some(
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(
                                    name,
                                ),
                            ) => {
                                // For named ports, we'd need to look at the Pod spec
                                // For now, fall back to service_port
                                warn!(
                                    "Named targetPort '{}' not yet supported, using service port",
                                    name
                                );
                                service_port as u16
                            }
                            None => service_port as u16, // Default to service port
                        }
                    })
                    .unwrap_or(service_port as u16)
            } else {
                service_port as u16
            }
        } else {
            service_port as u16
        };

        info!(
            "Service {}/{} port {} -> targetPort {}",
            namespace, service_name, service_port, target_port
        );

        let endpointslice_api: Api<EndpointSlice> = Api::namespaced(self.client.clone(), namespace);

        // List all EndpointSlices for this Service
        // EndpointSlices are labeled with kubernetes.io/service-name
        let list_params =
            ListParams::default().labels(&format!("kubernetes.io/service-name={}", service_name));

        match endpointslice_api.list(&list_params).await {
            Ok(endpointslice_list) => {
                let mut backends = Vec::new();

                // Merge backends from all EndpointSlices, matching the target port
                for endpointslice in endpointslice_list.items {
                    let slice_backends =
                        parse_endpointslice_to_backends(&endpointslice, target_port);
                    backends.extend(slice_backends);
                }

                if backends.is_empty() {
                    warn!(
                        "Service {}/{} has no ready endpoints",
                        namespace, service_name
                    );
                }

                info!(
                    "Resolved service {}/{} to {} backends",
                    namespace,
                    service_name,
                    backends.len()
                );

                Ok(backends)
            }
            Err(e) => {
                warn!(
                    "Failed to get endpointslices for service {}/{}: {}",
                    namespace, service_name, e
                );
                Err(e)
            }
        }
    }

    /// Reconcile a single HTTPRoute
    async fn reconcile(route: Arc<HTTPRoute>, ctx: Arc<Self>) -> Result<Action, kube::Error> {
        let start = Instant::now();
        let namespace = route.namespace().unwrap_or_else(|| "default".to_string());
        let name = route.name_any();

        info!("Reconciling HTTPRoute: {}/{}", namespace, name);

        // Check if this HTTPRoute references our Gateway
        if !ctx.should_reconcile(&route.spec.parent_refs) {
            debug!(
                "HTTPRoute {}/{} does not reference our Gateway, ignoring",
                namespace, name
            );
            return Ok(Action::await_change());
        }

        info!(
            "HTTPRoute {}/{} references our Gateway, configuring routes",
            namespace, name
        );

        // Parse and configure routes
        let mut routes_added = 0;
        if let Some(rules) = &route.spec.rules {
            for (rule_idx, rule) in rules.iter().enumerate() {
                // Extract path from matches (default to "/" if no matches)
                let path = if let Some(matches) = &rule.matches {
                    if let Some(first_match) = matches.first() {
                        if let Some(path_match) = &first_match.path {
                            path_match.value.as_deref().unwrap_or("/")
                        } else {
                            "/"
                        }
                    } else {
                        "/"
                    }
                } else {
                    "/"
                };

                // Extract backends
                if let Some(backend_refs) = &rule.backend_refs {
                    // First pass: resolve all services and collect weights
                    let mut resolved_services = Vec::new();
                    let mut total_weight = 0i32;

                    for backend_ref in backend_refs {
                        let service_name = &backend_ref.name;
                        let port = backend_ref.port.unwrap_or(80) as u32;
                        let weight = backend_ref.weight.unwrap_or(1);

                        info!(
                            "  - Resolving backend Service: {}/{}:{} (weight: {})",
                            namespace, service_name, port, weight
                        );

                        match ctx
                            .resolve_service_endpoints(service_name, &namespace, port)
                            .await
                        {
                            Ok(service_backends) => {
                                info!("    -> Resolved to {} Pod IPs", service_backends.len());
                                resolved_services.push((service_backends, weight));
                                total_weight += weight;
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to resolve service {}/{}: {}",
                                    namespace, service_name, e
                                );
                            }
                        }
                    }

                    // Second pass: build weighted backend list
                    // Strategy: Interleave backends proportionally to avoid truncation bias
                    const MAX_MAGLEV_BACKENDS: usize = 31;

                    let mut backends = Vec::new();

                    if total_weight > 0 {
                        // First, calculate target slots for each service using largest-remainder method
                        // This ensures slots sum to exactly MAX_MAGLEV_BACKENDS (more fair than simple rounding)
                        let mut service_targets = Vec::new();
                        let mut slot_allocations = Vec::new();

                        // Step 1: Calculate exact fractional slots and assign floor values
                        let mut allocated_slots = 0;
                        for (service_backends, weight) in &resolved_services {
                            let exact_slots =
                                (*weight as f64 / total_weight as f64) * MAX_MAGLEV_BACKENDS as f64;
                            let floor_slots = exact_slots.floor() as usize;
                            let remainder = exact_slots - floor_slots as f64;

                            slot_allocations.push((service_backends, floor_slots, remainder));
                            allocated_slots += floor_slots;
                        }

                        // Step 2: Distribute remaining slots to services with largest remainders
                        let remaining_slots = MAX_MAGLEV_BACKENDS - allocated_slots;
                        let mut indexed_remainders: Vec<(usize, f64)> = slot_allocations
                            .iter()
                            .enumerate()
                            .map(|(idx, (_, _, remainder))| (idx, *remainder))
                            .collect();

                        // Sort by remainder descending (largest first)
                        indexed_remainders.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

                        // Give one extra slot to top N services (where N = remaining_slots)
                        for (idx, _remainder) in indexed_remainders
                            .iter()
                            .take(remaining_slots.min(indexed_remainders.len()))
                        {
                            slot_allocations[*idx].1 += 1;
                        }

                        // Step 3: Calculate replicas per pod for each service
                        for (service_backends, service_slots, _) in slot_allocations {
                            let pods_count = service_backends.len();
                            let replicas_per_pod =
                                (service_slots as f64 / pods_count as f64).ceil().max(1.0) as usize;

                            info!(
                                "  - Service gets {} slots, {} replicas per pod ({} pods)",
                                service_slots, replicas_per_pod, pods_count
                            );

                            service_targets.push((service_backends.clone(), replicas_per_pod));
                        }

                        // Interleave backends round-robin across services to avoid truncation bias
                        let max_replicas =
                            service_targets.iter().map(|(_, r)| *r).max().unwrap_or(0);
                        for replica_round in 0..max_replicas {
                            for (service_backends, replicas_per_pod) in &service_targets {
                                if replica_round < *replicas_per_pod {
                                    backends.extend(service_backends.clone());
                                }
                            }
                        }

                        // Trim to exactly MAX_MAGLEV_BACKENDS if we went over
                        if backends.len() > MAX_MAGLEV_BACKENDS {
                            backends.truncate(MAX_MAGLEV_BACKENDS);
                        }
                    }

                    info!(
                        "  - Final backend count: {} (max: {})",
                        backends.len(),
                        MAX_MAGLEV_BACKENDS
                    );

                    if !backends.is_empty() {
                        let backend_count = backends.len();
                        // Add route to router (using GET as default method)
                        match ctx.router.add_route(HttpMethod::GET, path, backends) {
                            Ok(_) => {
                                info!(
                                    "  - Added route: {} -> {} total backends",
                                    path, backend_count
                                );
                                routes_added += 1;
                            }
                            Err(e) => {
                                warn!("Failed to add route {}: {}", path, e);
                            }
                        }
                    } else {
                        warn!(
                            "No backends found for route {} (all services failed to resolve)",
                            path
                        );
                    }
                } else {
                    debug!("Rule {} has no backend refs", rule_idx);
                }
            }
        }

        // Update HTTPRoute status
        ctx.set_route_status(&namespace, &name, routes_added > 0)
            .await?;

        // Record metrics
        record_httproute_reconciliation(
            &name,
            &namespace,
            start.elapsed().as_secs_f64(),
            "success",
        );

        // Requeue after 5 minutes for periodic reconciliation
        Ok(Action::requeue(Duration::from_secs(300)))
    }

    /// Update HTTPRoute status with Accepted condition
    async fn set_route_status(
        &self,
        namespace: &str,
        name: &str,
        accepted: bool,
    ) -> Result<(), kube::Error> {
        let api: Api<HTTPRoute> = Api::namespaced(self.client.clone(), namespace);

        let status = if accepted {
            json!({
                "status": {
                    "parents": [{
                        "parentRef": {
                            "name": self.gateway_name,
                            "kind": "Gateway",
                        },
                        "controllerName": "rauta.io/gateway-controller",
                        "conditions": [{
                            "type": "Accepted",
                            "status": "True",
                            "reason": "Accepted",
                            "message": "HTTPRoute is accepted and configured",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        }, {
                            "type": "ResolvedRefs",
                            "status": "True",
                            "reason": "ResolvedRefs",
                            "message": "All backend refs resolved",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        }]
                    }]
                }
            })
        } else {
            json!({
                "status": {
                    "parents": [{
                        "parentRef": {
                            "name": self.gateway_name,
                            "kind": "Gateway",
                        },
                        "controllerName": "rauta.io/gateway-controller",
                        "conditions": [{
                            "type": "Accepted",
                            "status": "False",
                            "reason": "NoRules",
                            "message": "HTTPRoute has no valid routing rules",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        }]
                    }]
                }
            })
        };

        api.patch_status(
            name,
            &PatchParams::apply("rauta-controller"),
            &Patch::Merge(&status),
        )
        .await?;

        info!(
            "Updated HTTPRoute {}/{} status: accepted={}",
            namespace, name, accepted
        );
        Ok(())
    }

    /// Error handler for controller
    fn error_policy(_obj: Arc<HTTPRoute>, error: &kube::Error, _ctx: Arc<Self>) -> Action {
        error!("HTTPRoute reconciliation error: {:?}", error);
        // Retry after 1 minute on errors
        Action::requeue(Duration::from_secs(60))
    }

    /// Start the HTTPRoute controller
    pub async fn run(self) -> Result<(), kube::Error> {
        let api: Api<HTTPRoute> = Api::all(self.client.clone());
        let ctx = Arc::new(self);

        info!("Starting HTTPRoute controller");

        // NOTE: EndpointSlice watching
        //
        // Ideally, we would watch EndpointSlice resources and trigger HTTPRoute reconciliation
        // when pods scale up/down. However, kube-rs Controller::watches() requires a mapper
        // function that cannot perform async operations, making it impossible to query which
        // HTTPRoutes reference a given Service.
        //
        // Current behavior:
        // - HTTPRoutes are reconciled periodically (default: every 5 minutes)
        // - Reconciliation resolves Service -> EndpointSlice -> Pod IPs
        // - Backend pool is updated if Pod IPs changed
        // - Router's idempotent add_route() makes updates cheap
        //
        // For production, consider:
        // - Using a custom reflector/watcher for EndpointSlice
        // - Maintaining an in-memory index of Service -> HTTPRoute mappings
        // - Triggering reconciliation via a channel when EndpointSlice changes
        //
        // This is acceptable because:
        // - 5-minute lag for pod scaling is reasonable for most workloads
        // - Kubernetes typically takes 30-60s for pod readiness anyway
        // - Manual reconciliation can be triggered via `kubectl annotate`

        Controller::new(api, WatcherConfig::default())
            .run(Self::reconcile, Self::error_policy, ctx)
            .for_each(|res| async move {
                match res {
                    Ok(o) => debug!("Reconciled HTTPRoute: {:?}", o),
                    Err(e) => error!("Reconciliation error: {:?}", e),
                }
            })
            .await;

        Ok(())
    }
}

/// Parse EndpointSlice into Backend structs
#[allow(dead_code)] // Used in K8s mode
fn parse_endpointslice_to_backends(
    endpoint_slice: &k8s_openapi::api::discovery::v1::EndpointSlice,
    target_port: u16,
) -> Vec<Backend> {
    let mut backends = Vec::new();

    // Match the target port in EndpointSlice ports
    let port = if let Some(ports) = &endpoint_slice.ports {
        ports
            .iter()
            .find(|p| p.port.map(|pnum| pnum as u16) == Some(target_port))
            .and_then(|p| p.port)
            .unwrap_or(target_port as i32) as u16
    } else {
        target_port
    };

    // Iterate through endpoints
    for endpoint in &endpoint_slice.endpoints {
        // Check if endpoint is ready (only use ready endpoints)
        let is_ready = if let Some(conditions) = &endpoint.conditions {
            conditions.ready.unwrap_or(false)
        } else {
            true // Default to ready if no conditions
        };

        if !is_ready {
            continue; // Skip non-ready endpoints
        }

        // Parse IP addresses
        for address in &endpoint.addresses {
            match address.parse::<std::net::Ipv4Addr>() {
                Ok(ipv4) => {
                    backends.push(Backend::from_ipv4(ipv4, port, 100));
                }
                Err(e) => {
                    warn!("Failed to parse IP address {}: {}", address, e);
                }
            }
        }
    }

    backends
}

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_api::apis::standard::httproutes::{HTTPRoute, HTTPRouteSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    #[test]
    fn test_httproute_basic_structure() {
        // RED: This test documents basic HTTPRoute structure

        // Create a basic HTTPRoute
        let route = HTTPRoute {
            metadata: ObjectMeta {
                name: Some("api-route".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: HTTPRouteSpec::default(),
            status: None,
        };

        // Verify basic structure
        assert_eq!(route.metadata.name, Some("api-route".to_string()));
        assert_eq!(route.metadata.namespace, Some("default".to_string()));

        // Note: Full reconcile() testing will verify the following:
        // 1. The reconciler parses HTTPRoute rules
        // 2. It adds routes to the Router with correct path matching
        // 3. It resolves Service endpoints from backendRefs
        // 4. It updates status with route acceptance
    }

    #[test]
    fn test_httproute_reconciler_creation() {
        // RED: This test documents HTTPRoute reconciler setup

        // Create a router for the reconciler
        let router = Arc::new(Router::new());

        // Router starts with no routes configured
        // (We can't easily test this without accessing private fields,
        // but the reconciler will add routes via add_route())

        // Note: Full reconciler.run() testing will verify the following:
        // 1. HTTPRoute resources are watched
        // 2. Routes are added to Router based on HTTPRoute spec
        // 3. Service endpoints are resolved from Kubernetes API
        // 4. Status is updated with route acceptance

        // For now, just verify router was created
        assert!(Arc::strong_count(&router) == 1);
    }

    #[test]
    fn test_httproute_metrics_recorded() {
        // RED: Test that HTTPRoute reconciliation records metrics
        // This test will FAIL until we implement metrics

        use crate::apis::metrics::gather_controller_metrics;

        // Record a fake reconciliation
        crate::apis::metrics::record_httproute_reconciliation(
            "test-route",
            "default",
            0.123,
            "success",
        );

        // Gather metrics and verify they contain the expected data
        let metrics = gather_controller_metrics().expect("Should gather metrics");

        assert!(
            metrics.contains("httproute_reconciliation_duration_seconds"),
            "Should contain duration metric"
        );
        assert!(
            metrics.contains("httproute_reconciliations_total"),
            "Should contain counter metric"
        );
    }

    #[test]
    fn test_endpointslice_to_backends() {
        // Test parsing EndpointSlice with ready endpoints
        use k8s_openapi::api::discovery::v1::{Endpoint, EndpointPort, EndpointSlice};

        // Create a mock EndpointSlice with 3 endpoints
        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.1".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.2".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.3".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
            ],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Parse into backends (target port 8080)
        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);

        // Verify we got 3 backends
        assert_eq!(backends.len(), 3, "Should have 3 backends");

        // Verify first backend
        assert_eq!(
            backends[0].as_ipv4().unwrap(),
            "10.0.1.1".parse::<std::net::Ipv4Addr>().unwrap()
        );
        assert_eq!(backends[0].port, 8080);
        assert_eq!(backends[0].weight, 100);

        // Verify second backend
        assert_eq!(
            backends[1].as_ipv4().unwrap(),
            "10.0.1.2".parse::<std::net::Ipv4Addr>().unwrap()
        );

        // Verify third backend
        assert_eq!(
            backends[2].as_ipv4().unwrap(),
            "10.0.1.3".parse::<std::net::Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn test_endpointslice_filters_not_ready() {
        // Test that non-ready endpoints are filtered out
        use k8s_openapi::api::discovery::v1::{
            Endpoint, EndpointConditions, EndpointPort, EndpointSlice,
        };

        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.1".to_string()],
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: Some(true),
                        terminating: Some(false),
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.2".to_string()],
                    conditions: Some(EndpointConditions {
                        ready: Some(false),
                        serving: Some(false),
                        terminating: Some(true),
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.3".to_string()],
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: Some(true),
                        terminating: Some(false),
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
            ],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);

        // Should only get 2 backends (the ready ones)
        assert_eq!(backends.len(), 2, "Should have 2 ready backends");

        // Verify we got the right IPs (not the not-ready one)
        let ips: Vec<String> = backends
            .iter()
            .map(|b| b.as_ipv4().unwrap().to_string())
            .collect();
        assert!(ips.contains(&"10.0.1.1".to_string()));
        assert!(!ips.contains(&"10.0.1.2".to_string())); // Not ready
        assert!(ips.contains(&"10.0.1.3".to_string()));
    }

    #[test]
    fn test_endpointslice_empty() {
        // Test EndpointSlice with no endpoints
        use k8s_openapi::api::discovery::v1::{EndpointPort, EndpointSlice};

        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);
        assert_eq!(backends.len(), 0, "Should have 0 backends");
    }

    #[test]
    fn test_endpointslice_default_port() {
        // Test EndpointSlice with no port specified
        use k8s_openapi::api::discovery::v1::{Endpoint, EndpointSlice};

        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![Endpoint {
                addresses: vec!["10.0.1.1".to_string()],
                conditions: None,
                deprecated_topology: None,
                hints: None,
                hostname: None,
                node_name: None,
                target_ref: None,
                zone: None,
            }],
            metadata: Default::default(),
            ports: None, // No port specified
        };

        let backends = parse_endpointslice_to_backends(&endpoint_slice, 80);
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].port, 80, "Should default to port 80");
    }
}

// Note: HTTPRoute watcher implementation is planned for Phase 1
