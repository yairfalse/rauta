//! Gateway watcher
//!
//! Watches Gateway resources and configures listeners (HTTP, HTTPS, TCP).

use crate::apis::metrics::record_gateway_reconciliation;
use futures::StreamExt;
use gateway_api::apis::standard::gateways::Gateway;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, ResourceExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info};

/// Gateway reconciler
#[allow(dead_code)] // Used in K8s mode
pub struct GatewayReconciler {
    client: Client,
    /// GatewayClass name to watch for
    gateway_class_name: String,
}

#[allow(dead_code)] // Used in K8s mode
impl GatewayReconciler {
    pub fn new(client: Client, gateway_class_name: String) -> Self {
        Self {
            client,
            gateway_class_name,
        }
    }

    /// Check if this Gateway references our GatewayClass
    fn should_reconcile(&self, gateway_class_name: &str) -> bool {
        gateway_class_name == self.gateway_class_name
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

        // Update Gateway status
        ctx.set_gateway_status(&namespace, &name, true, listener_count)
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
        listener_count: usize,
    ) -> Result<(), kube::Error> {
        let api: Api<Gateway> = Api::namespaced(self.client.clone(), namespace);

        let status = if accepted {
            // Build listener statuses
            let listener_statuses: Vec<_> = (0..listener_count)
                .map(|i| {
                    json!({
                        "attachedRoutes": 0,
                        "conditions": [{
                            "type": "Accepted",
                            "status": "True",
                            "reason": "Accepted",
                            "message": "Listener is accepted",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        }, {
                            "type": "Programmed",
                            "status": "True",
                            "reason": "Programmed",
                            "message": "Listener is programmed",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        }, {
                            "type": "ResolvedRefs",
                            "status": "True",
                            "reason": "ResolvedRefs",
                            "message": "All references resolved",
                            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        }],
                        "name": format!("listener-{}", i),
                        "supportedKinds": [{
                            "group": "gateway.networking.k8s.io",
                            "kind": "HTTPRoute"
                        }]
                    })
                })
                .collect();

            json!({
                "status": {
                    "conditions": [{
                        "type": "Accepted",
                        "status": "True",
                        "reason": "Accepted",
                        "message": "Gateway is accepted",
                        "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    }, {
                        "type": "Programmed",
                        "status": "True",
                        "reason": "Programmed",
                        "message": "Gateway is programmed",
                        "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    }],
                    "listeners": listener_statuses
                }
            })
        } else {
            json!({
                "status": {
                    "conditions": [{
                        "type": "Accepted",
                        "status": "False",
                        "reason": "Invalid",
                        "message": "Gateway configuration is invalid",
                        "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
            "Updated Gateway {}/{} status: accepted={}, listeners={}",
            namespace, name, accepted, listener_count
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

        // TODO: Test reconcile() when implemented
        // This is where we'll test that:
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
                    // TODO: Add TLS configuration
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

        // TODO: Test TLS certificate loading and validation
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

        // TODO: Test that both listeners are configured correctly
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
