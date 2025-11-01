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
pub struct HTTPRouteReconciler {
    client: Client,
    router: Arc<Router>,
    /// Gateway name to watch routes for
    gateway_name: String,
}

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
        _service_port: u32,
    ) -> Result<Vec<Backend>, kube::Error> {
        use k8s_openapi::api::discovery::v1::EndpointSlice;
        use kube::api::ListParams;

        let endpointslice_api: Api<EndpointSlice> = Api::namespaced(self.client.clone(), namespace);

        // List all EndpointSlices for this Service
        // EndpointSlices are labeled with kubernetes.io/service-name
        let list_params =
            ListParams::default().labels(&format!("kubernetes.io/service-name={}", service_name));

        match endpointslice_api.list(&list_params).await {
            Ok(endpointslice_list) => {
                let mut backends = Vec::new();

                // Merge backends from all EndpointSlices
                for endpointslice in endpointslice_list.items {
                    let slice_backends = parse_endpointslice_to_backends(&endpointslice);
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
                    let mut backends = Vec::new();

                    // Resolve each backend Service to Pod IPs via EndpointSlice
                    for backend_ref in backend_refs {
                        let service_name = &backend_ref.name;
                        let port = backend_ref.port.unwrap_or(80) as u32;

                        info!(
                            "  - Resolving backend Service: {}/{}:{}",
                            namespace, service_name, port
                        );

                        // Resolve Service to Pod IPs
                        match ctx
                            .resolve_service_endpoints(service_name, &namespace, port)
                            .await
                        {
                            Ok(service_backends) => {
                                info!("    -> Resolved to {} Pod IPs", service_backends.len());
                                backends.extend(service_backends);
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to resolve service {}/{}: {}",
                                    namespace, service_name, e
                                );
                                // Continue with other backends even if this one fails
                            }
                        }
                    }

                    if !backends.is_empty() {
                        // Add route to router (using GET as default method)
                        match ctx
                            .router
                            .add_route(HttpMethod::GET, path, backends.clone())
                        {
                            Ok(_) => {
                                info!(
                                    "  - Added route: {} -> {} total backends",
                                    path,
                                    backends.len()
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
fn parse_endpointslice_to_backends(
    endpoint_slice: &k8s_openapi::api::discovery::v1::EndpointSlice,
) -> Vec<Backend> {
    let mut backends = Vec::new();

    // Get port from EndpointSlice (default to 80 if not specified)
    let port = if let Some(ports) = &endpoint_slice.ports {
        ports.first().and_then(|p| p.port).unwrap_or(80) as u16
    } else {
        80
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
                    backends.push(Backend {
                        ipv4: u32::from(ipv4),
                        port,
                        weight: 100, // Default weight
                    });
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

        // TODO: Test reconcile() when implemented
        // This is where we'll test that:
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

        // TODO: Test reconciler.run() when implemented
        // This is where we'll test that:
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

        // Parse into backends
        let backends = parse_endpointslice_to_backends(&endpoint_slice);

        // Verify we got 3 backends
        assert_eq!(backends.len(), 3, "Should have 3 backends");

        // Verify first backend
        assert_eq!(
            std::net::Ipv4Addr::from(backends[0].ipv4),
            "10.0.1.1".parse::<std::net::Ipv4Addr>().unwrap()
        );
        assert_eq!(backends[0].port, 8080);
        assert_eq!(backends[0].weight, 100);

        // Verify second backend
        assert_eq!(
            std::net::Ipv4Addr::from(backends[1].ipv4),
            "10.0.1.2".parse::<std::net::Ipv4Addr>().unwrap()
        );

        // Verify third backend
        assert_eq!(
            std::net::Ipv4Addr::from(backends[2].ipv4),
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

        let backends = parse_endpointslice_to_backends(&endpoint_slice);

        // Should only get 2 backends (the ready ones)
        assert_eq!(backends.len(), 2, "Should have 2 ready backends");

        // Verify we got the right IPs (not the not-ready one)
        let ips: Vec<String> = backends
            .iter()
            .map(|b| std::net::Ipv4Addr::from(b.ipv4).to_string())
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

        let backends = parse_endpointslice_to_backends(&endpoint_slice);
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

        let backends = parse_endpointslice_to_backends(&endpoint_slice);
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].port, 80, "Should default to port 80");
    }
}

// TODO: Implement HTTPRoute watcher (Phase 1)
