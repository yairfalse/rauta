//! Gateway watcher
//!
//! Watches Gateway resources and configures listeners (HTTP, HTTPS, TCP).

use crate::apis::metrics::record_gateway_reconciliation;
use crate::proxy::listener_manager::{ListenerConfig, ListenerManager, Protocol};
use futures::StreamExt;
use gateway_api::apis::standard::gateways::Gateway;
use k8s_openapi::api::core::v1::Node;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, ResourceExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Gateway reconciler
#[allow(dead_code)] // Used in K8s mode
pub struct GatewayReconciler {
    client: Client,
    /// GatewayClass name to watch for
    gateway_class_name: String,
    /// Listener manager for dynamic listener lifecycle
    listener_manager: Arc<ListenerManager>,
}

#[allow(dead_code)] // Used in K8s mode
impl GatewayReconciler {
    pub fn new(
        client: Client,
        gateway_class_name: String,
        listener_manager: Arc<ListenerManager>,
    ) -> Self {
        Self {
            client,
            gateway_class_name,
            listener_manager,
        }
    }

    /// Check if this Gateway references our GatewayClass
    fn should_reconcile(&self, gateway_class_name: &str) -> bool {
        gateway_class_name == self.gateway_class_name
    }

    /// Get node IPs for Gateway status addresses
    /// Returns all node InternalIP addresses where RAUTA could be running
    async fn get_node_addresses(&self) -> Vec<serde_json::Value> {
        let nodes_api: Api<Node> = Api::all(self.client.clone());

        match nodes_api.list(&Default::default()).await {
            Ok(nodes) => {
                let mut addresses = Vec::new();

                for node in nodes.items {
                    // Get node's InternalIP
                    if let Some(status) = &node.status {
                        if let Some(addrs) = &status.addresses {
                            for addr in addrs {
                                if addr.type_ == "InternalIP" {
                                    addresses.push(json!({
                                        "type": "IPAddress",
                                        "value": addr.address
                                    }));
                                }
                            }
                        }
                    }
                }

                if addresses.is_empty() {
                    warn!("No node addresses found, Gateway will not have addresses in status");
                }

                addresses
            }
            Err(e) => {
                warn!("Failed to list nodes: {:?}", e);
                Vec::new()
            }
        }
    }

    /// Reconcile a single Gateway
    async fn reconcile(gateway: Arc<Gateway>, ctx: Arc<Self>) -> Result<Action, kube::Error> {
        let start = Instant::now();
        let namespace = gateway.namespace().unwrap_or_else(|| "default".to_string());
        let name = gateway.name_any();
        let gateway_class = &gateway.spec.gateway_class_name;

        info!("Reconciling Gateway: {}/{}", namespace, name);

        // Check if this Gateway references our GatewayClass
        if !ctx.should_reconcile(gateway_class) {
            debug!(
                "Gateway {}/{} references GatewayClass '{}', ignoring",
                namespace, name, gateway_class
            );
            return Ok(Action::await_change());
        }

        info!(
            "Gateway {}/{} references our GatewayClass, configuring listeners",
            namespace, name
        );

        // Configure listeners
        let listener_count = gateway.spec.listeners.len();
        info!(
            "Configuring {} listener(s) for Gateway {}/{}",
            listener_count, namespace, name
        );

        for listener in &gateway.spec.listeners {
            info!(
                "  - Listener '{}': {}:{}",
                listener.name, listener.protocol, listener.port
            );
        }

        // Configure listeners in ListenerManager
        for listener in &gateway.spec.listeners {
            // Convert protocol string to Protocol enum
            let protocol = match listener.protocol.as_str() {
                "HTTP" => Protocol::HTTP,
                "HTTPS" => Protocol::HTTPS,
                _ => {
                    warn!(
                        "Unsupported protocol '{}' for listener '{}' in Gateway {}/{}",
                        listener.protocol, listener.name, namespace, name
                    );
                    continue; // Skip unsupported protocols
                }
            };

            let config = ListenerConfig {
                name: listener.name.clone(),
                protocol,
                port: listener.port as u16,
                hostname: listener.hostname.clone(),
                gateway_namespace: namespace.clone(),
                gateway_name: name.clone(),
            };

            match ctx.listener_manager.upsert_listener(config).await {
                Ok(listener_id) => {
                    info!(
                        "Configured listener {} for Gateway {}/{}",
                        listener_id, namespace, name
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to configure listener '{}' for Gateway {}/{}: {}",
                        listener.name, namespace, name, e
                    );
                    // Continue with other listeners even if one fails
                    // TODO: Update listener status to reflect failure
                }
            }
        }

        // Update Gateway status
        let generation = gateway.metadata.generation.unwrap_or(0);
        ctx.set_gateway_status(&namespace, &name, true, &gateway, generation)
            .await?;

        // Record metrics
        record_gateway_reconciliation(&name, &namespace, start.elapsed().as_secs_f64(), "success");

        // Requeue after 5 minutes for periodic reconciliation
        Ok(Action::requeue(Duration::from_secs(300)))
    }

    /// Update Gateway status with Accepted and Programmed conditions
    async fn set_gateway_status(
        &self,
        namespace: &str,
        name: &str,
        accepted: bool,
        gateway: &Gateway,
        generation: i64,
    ) -> Result<(), kube::Error> {
        let api: Api<Gateway> = Api::namespaced(self.client.clone(), namespace);
        // Get node addresses for Gateway status
        let addresses = self.get_node_addresses().await;

        let status = if accepted {
            // Build listener statuses using actual listener names
            let listener_statuses: Vec<_> = gateway.spec.listeners.iter()
                .map(|listener| {
                    // RAUTA currently only supports HTTPRoute
                    let rauta_supported_kinds = ["HTTPRoute"];

                    // Validate listener.allowedRoutes.kinds
                    let (resolved_refs_status, resolved_refs_reason, resolved_refs_message, supported_kinds) =
                        if let Some(allowed_routes) = &listener.allowed_routes {
                            if let Some(kinds) = &allowed_routes.kinds {
                                if kinds.is_empty() {
                                    // Empty kinds list is invalid
                                    ("False", "InvalidRouteKinds", "No route kinds specified", vec![])
                                } else {
                                    // Filter to only supported kinds
                                    let valid_kinds: Vec<_> = kinds.iter()
                                        .filter(|k| rauta_supported_kinds.contains(&k.kind.as_str()))
                                        .collect();

                                    if valid_kinds.is_empty() {
                                        // No supported kinds found
                                        ("False", "InvalidRouteKinds", "No supported route kinds found", vec![])
                                    } else {
                                        // At least one supported kind found
                                        let supported: Vec<_> = valid_kinds.iter()
                                            .map(|k| {
                                                json!({
                                                    "group": k.group.as_ref().unwrap_or(&"gateway.networking.k8s.io".to_string()),
                                                    "kind": k.kind.clone()
                                                })
                                            })
                                            .collect();
                                        ("True", "ResolvedRefs", "All references resolved", supported)
                                    }
                                }
                            } else {
                                // No kinds specified, use default based on protocol
                                ("True", "ResolvedRefs", "All references resolved", vec![
                                    json!({
                                        "group": "gateway.networking.k8s.io",
                                        "kind": "HTTPRoute"
                                    })
                                ])
                            }
                        } else {
                            // No allowedRoutes specified, use default based on protocol
                            ("True", "ResolvedRefs", "All references resolved", vec![
                                json!({
                                    "group": "gateway.networking.k8s.io",
                                    "kind": "HTTPRoute"
                                })
                            ])
                        };

                    json!({
                        "attachedRoutes": 0,
                        "conditions": [{
                            "type": "Accepted",
                            "status": "True",
                            "reason": "Accepted",
                            "message": "Listener is accepted",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            "observedGeneration": generation,
                        }, {
                            "type": "Programmed",
                            "status": "True",
                            "reason": "Programmed",
                            "message": "Listener is programmed",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            "observedGeneration": generation,
                        }, {
                            "type": "ResolvedRefs",
                            "status": resolved_refs_status,
                            "reason": resolved_refs_reason,
                            "message": resolved_refs_message,
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            "observedGeneration": generation,
                        }],
                        "name": listener.name.clone(),
                        "supportedKinds": supported_kinds
                    })
                })
                .collect();

            json!({
                "addresses": addresses,
                "conditions": [{
                    "type": "Accepted",
                    "status": "True",
                    "reason": "Accepted",
                    "message": "Gateway is accepted",
                    "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "observedGeneration": generation,
                }, {
                    "type": "Programmed",
                    "status": "True",
                    "reason": "Programmed",
                    "message": "Gateway is programmed",
                    "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "observedGeneration": generation,
                }],
                "listeners": listener_statuses
            })
        } else {
            json!({
                "conditions": [{
                    "type": "Accepted",
                    "status": "False",
                    "reason": "Invalid",
                    "message": "Gateway configuration is invalid",
                    "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "observedGeneration": generation,
                }]
            })
        };

        api.patch_status(name, &PatchParams::default(), &Patch::Merge(&status))
            .await?;

        info!(
            "Updated Gateway {}/{} status: accepted={}, listeners={}",
            namespace,
            name,
            accepted,
            gateway.spec.listeners.len()
        );
        Ok(())
    }

    /// Error handler for controller
    fn error_policy(_obj: Arc<Gateway>, error: &kube::Error, _ctx: Arc<Self>) -> Action {
        error!("Gateway reconciliation error: {:?}", error);
        // Retry after 1 minute on errors
        Action::requeue(Duration::from_secs(60))
    }

    /// Start the Gateway controller
    pub async fn run(self) -> Result<(), kube::Error> {
        let api: Api<Gateway> = Api::all(self.client.clone());
        let ctx = Arc::new(self);

        info!("Starting Gateway controller");

        Controller::new(api, WatcherConfig::default())
            .run(Self::reconcile, Self::error_policy, ctx)
            .for_each(|res| async move {
                match res {
                    Ok(o) => debug!("Reconciled Gateway: {:?}", o),
                    Err(e) => error!("Reconciliation error: {:?}", e),
                }
            })
            .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use gateway_api::apis::standard::gateways::{Gateway, GatewayListeners, GatewaySpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    #[test]
    fn test_gateway_with_http_listener() {
        // RED: This test documents the Gateway API structure for HTTP listeners

        // Create a Gateway with HTTP listener on port 80
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("rauta-gateway".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "rauta".to_string(),
                listeners: vec![GatewayListeners {
                    name: "http".to_string(),
                    port: 80,
                    protocol: "HTTP".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            status: None,
        };

        // Verify the Gateway references our GatewayClass
        assert_eq!(gateway.spec.gateway_class_name, "rauta");

        // Verify HTTP listener configuration
        assert_eq!(gateway.spec.listeners.len(), 1);
        let listener = &gateway.spec.listeners[0];
        assert_eq!(listener.name, "http");
        assert_eq!(listener.port, 80);
        assert_eq!(listener.protocol, "HTTP");

        // Note: Full reconcile() testing will verify the following:
        // 1. The reconciler accepts this Gateway
        // 2. It configures the HTTP listener on port 80
        // 3. It sets status.conditions with Accepted=true, Programmed=true
        // 4. It sets status.listeners with Ready=true for the HTTP listener
    }

    #[test]
    fn test_gateway_with_https_listener() {
        // RED: This test documents HTTPS listener configuration

        // Create a Gateway with HTTPS listener on port 443
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("rauta-gateway-tls".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "rauta".to_string(),
                listeners: vec![GatewayListeners {
                    name: "https".to_string(),
                    port: 443,
                    protocol: "HTTPS".to_string(),
                    // Note: TLS configuration will be added in future
                    ..Default::default()
                }],
                ..Default::default()
            },
            status: None,
        };

        // Verify HTTPS listener configuration
        assert_eq!(gateway.spec.listeners.len(), 1);
        let listener = &gateway.spec.listeners[0];
        assert_eq!(listener.name, "https");
        assert_eq!(listener.port, 443);
        assert_eq!(listener.protocol, "HTTPS");

        // Verify this compiles (TLS config structure exists in Gateway API)
        // Actual TLS parsing will be implemented in reconciler
    }

    #[test]
    fn test_gateway_with_multiple_listeners() {
        // RED: This test documents multiple listeners (HTTP + HTTPS)

        // Create a Gateway with both HTTP and HTTPS listeners
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("rauta-gateway-dual".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "rauta".to_string(),
                listeners: vec![
                    GatewayListeners {
                        name: "http".to_string(),
                        port: 80,
                        protocol: "HTTP".to_string(),
                        ..Default::default()
                    },
                    GatewayListeners {
                        name: "https".to_string(),
                        port: 443,
                        protocol: "HTTPS".to_string(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            status: None,
        };

        // Verify multiple listeners
        assert_eq!(gateway.spec.listeners.len(), 2);
        assert_eq!(gateway.spec.listeners[0].port, 80);
        assert_eq!(gateway.spec.listeners[1].port, 443);

        // Note: Full listener configuration testing planned
    }

    #[test]
    fn test_gateway_different_class() {
        // Verify we can detect Gateways for other GatewayClasses
        let gateway = Gateway {
            metadata: ObjectMeta {
                name: Some("nginx-gateway".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: GatewaySpec {
                gateway_class_name: "nginx".to_string(),
                listeners: vec![GatewayListeners {
                    name: "http".to_string(),
                    port: 80,
                    protocol: "HTTP".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            status: None,
        };

        // Should NOT match our GatewayClass
        assert_ne!(gateway.spec.gateway_class_name, "rauta");
    }

    #[test]
    fn test_gateway_metrics_recorded() {
        // RED: Test that Gateway reconciliation records metrics
        use crate::apis::metrics::gather_controller_metrics;

        // Record a fake reconciliation
        crate::apis::metrics::record_gateway_reconciliation(
            "test-gateway",
            "default",
            0.045,
            "success",
        );

        // Gather metrics and verify
        let metrics = gather_controller_metrics().expect("Should gather metrics");

        assert!(
            metrics.contains("gateway_reconciliation_duration_seconds"),
            "Should contain duration metric"
        );
        assert!(
            metrics.contains("gateway_reconciliations_total"),
            "Should contain counter metric"
        );
    }
}
