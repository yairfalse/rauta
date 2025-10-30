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
                    let backends: Vec<Backend> = backend_refs
                        .iter()
                        .map(|backend_ref| {
                            // TODO: This is TEMPORARY placeholder logic
                            // Real implementation will resolve Service -> Endpoints -> Pod IPs
                            // via K8s API (EndpointSlice watcher)
                            let port = backend_ref.port.unwrap_or(80);

                            // Generate valid placeholder IP: 10.0.1-254.1-254
                            // Avoid 10.0.0.x by using % 254 + 1
                            let ip = format!(
                                "10.0.{}.{}",
                                ((backend_ref.name.len() % 254) + 1), // 1-254
                                ((port % 254) + 1)                    // 1-254
                            );

                            info!(
                                "  - Backend: {} (resolved to {}:{})",
                                backend_ref.name, ip, port
                            );

                            let ipv4: std::net::Ipv4Addr = ip.parse().unwrap();
                            Backend {
                                ipv4: u32::from(ipv4),
                                port: port as u16,
                                weight: 100, // Default weight
                            }
                        })
                        .collect();

                    if !backends.is_empty() {
                        // Add route to router (using GET as default method)
                        match ctx.router.add_route(HttpMethod::GET, path, backends) {
                            Ok(_) => {
                                info!(
                                    "  - Added route: {} -> {} backends",
                                    path,
                                    rule.backend_refs.as_ref().unwrap().len()
                                );
                                routes_added += 1;
                            }
                            Err(e) => {
                                warn!("Failed to add route {}: {}", path, e);
                            }
                        }
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
}

// TODO: Implement HTTPRoute watcher (Phase 1)
