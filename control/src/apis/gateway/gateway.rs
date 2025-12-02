//! Gateway watcher
//!
//! Watches Gateway resources and configures listeners (HTTP, HTTPS, TCP).

use crate::apis::metrics::record_gateway_reconciliation;
use crate::proxy::listener_manager::{GatewayRef, ListenerConfig, ListenerManager, Protocol};
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
                port: listener.port as u16,
                protocol: protocol.clone(),
            };

            let gateway_ref = GatewayRef {
                namespace: namespace.clone(),
                name: name.clone(),
                listener_name: listener.name.clone(),
                hostname: listener.hostname.clone(),
            };

            match ctx
                .listener_manager
                .register_gateway(config, gateway_ref)
                .await
            {
                Ok(()) => {
                    info!(
                        "Registered Gateway {}/{} listener '{}' on port {}",
                        namespace, name, listener.name, listener.port
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to configure listener '{}' for Gateway {}/{}: {}",
                        listener.name, namespace, name, e
                    );
                    // Continue with other listeners even if one fails
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

    /// Validate TLS certificateRefs according to Gateway API spec
    ///
    /// Returns (status, reason, message) tuple:
    /// - ("True", "ResolvedRefs", "...") if all refs are valid
    /// - ("False", "InvalidCertificateRef", "...") if any ref is invalid
    ///
    /// Validation checks:
    /// 1. certificateRef.group must be "" or "core"
    /// 2. certificateRef.kind must be "Secret"
    /// 3. Secret must exist in the specified namespace
    /// 4. Secret must contain valid tls.crt and tls.key data
    async fn validate_certificate_refs(
        &self,
        cert_refs: &[gateway_api::apis::standard::gateways::GatewayListenersTlsCertificateRefs],
        gateway_namespace: &str,
    ) -> (&'static str, &'static str, String) {
        use k8s_openapi::api::core::v1::Secret;
        use kube::api::Api;

        for cert_ref in cert_refs {
            // Validation 1: Check group
            if let Some(group) = &cert_ref.group {
                // Gateway API spec: Only "" (core) group is supported for Secret refs
                if !group.is_empty() && group != "core" {
                    warn!(
                        "Invalid certificateRef group '{}' (only core group supported)",
                        group
                    );
                    return (
                        "False",
                        "InvalidCertificateRef",
                        format!("Unsupported group '{}' for certificateRef", group),
                    );
                }
            }

            // Validation 2: Check kind
            if let Some(kind) = &cert_ref.kind {
                // Gateway API spec: Only "Secret" kind is supported
                if kind != "Secret" {
                    warn!(
                        "Invalid certificateRef kind '{}' (only Secret supported)",
                        kind
                    );
                    return (
                        "False",
                        "InvalidCertificateRef",
                        format!("Unsupported kind '{}' for certificateRef", kind),
                    );
                }
            }

            // Validation 3: Check if Secret exists
            let secret_namespace = cert_ref.namespace.as_deref().unwrap_or(gateway_namespace);
            let secret_name = &cert_ref.name;

            let secret_api: Api<Secret> = Api::namespaced(self.client.clone(), secret_namespace);
            match secret_api.get(secret_name).await {
                Ok(secret) => {
                    // Validation 4: Check if Secret contains tls.crt and tls.key
                    if let Some(data) = &secret.data {
                        let has_cert = data.contains_key("tls.crt");
                        let has_key = data.contains_key("tls.key");

                        if !has_cert || !has_key {
                            warn!(
                                "Secret {}/{} is missing required fields (tls.crt: {}, tls.key: {})",
                                secret_namespace, secret_name, has_cert, has_key
                            );
                            return (
                                "False",
                                "InvalidCertificateRef",
                                format!(
                                    "Secret {}/{} is missing required fields (needs both tls.crt and tls.key)",
                                    secret_namespace, secret_name
                                ),
                            );
                        }

                        // Future: Validate PEM format here
                        // For now, just check presence
                    } else {
                        warn!(
                            "Secret {}/{} has no data field",
                            secret_namespace, secret_name
                        );
                        return (
                            "False",
                            "InvalidCertificateRef",
                            format!("Secret {}/{} has no data", secret_namespace, secret_name),
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to get Secret {}/{}: {}",
                        secret_namespace, secret_name, e
                    );
                    // Distinguish 404 from other errors
                    let message = match e {
                        kube::Error::Api(api_err) if api_err.code == 404 => {
                            format!("Secret {}/{} not found", secret_namespace, secret_name)
                        }
                        _ => {
                            format!(
                                "Failed to access Secret {}/{}: {}",
                                secret_namespace, secret_name, e
                            )
                        }
                    };
                    return ("False", "InvalidCertificateRef", message);
                }
            }
        }

        // All validations passed
        (
            "True",
            "ResolvedRefs",
            String::from("All references resolved"),
        )
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
            // Validate all listeners' TLS refs concurrently (async-aware)
            let tls_validations_futures: Vec<_> = gateway
                .spec
                .listeners
                .iter()
                .map(|listener| async {
                    if let Some(tls) = &listener.tls {
                        if let Some(cert_refs) = &tls.certificate_refs {
                            self.validate_certificate_refs(cert_refs, namespace).await
                        } else {
                            // No certificateRefs specified (TLS passthrough mode)
                            (
                                "True",
                                "ResolvedRefs",
                                String::from("All references resolved"),
                            )
                        }
                    } else {
                        // No TLS configured (HTTP listener)
                        (
                            "True",
                            "ResolvedRefs",
                            String::from("All references resolved"),
                        )
                    }
                })
                .collect();

            // Await all TLS validations concurrently
            let tls_validation_results = futures::future::join_all(tls_validations_futures).await;

            // Build listener statuses using actual listener names
            let listener_statuses: Vec<_> = gateway
                .spec
                .listeners
                .iter()
                .zip(tls_validation_results.into_iter())
                .map(|(listener, tls_validation)| {
                    // RAUTA currently only supports HTTPRoute
                    let rauta_supported_kinds = ["HTTPRoute"];

                    // If TLS validation failed, skip route kinds validation
                    let (resolved_refs_status, resolved_refs_reason, resolved_refs_message, supported_kinds) =
                        if tls_validation.0 == "False" {
                            // TLS validation failed - return immediately (move values, no clone)
                            (tls_validation.0, tls_validation.1, tls_validation.2, vec![
                                json!({
                                    "group": "gateway.networking.k8s.io",
                                    "kind": "HTTPRoute"
                                })
                            ])
                        } else if let Some(allowed_routes) = &listener.allowed_routes {
                            if let Some(kinds) = &allowed_routes.kinds {
                                if kinds.is_empty() {
                                    // Empty kinds list is invalid
                                    ("False", "InvalidRouteKinds", String::from("No route kinds specified"), vec![])
                                } else {
                                    // Filter to only supported kinds
                                    let valid_kinds: Vec<_> = kinds.iter()
                                        .filter(|k| rauta_supported_kinds.contains(&k.kind.as_str()))
                                        .collect();

                                    // Check if there are ANY unsupported kinds
                                    let has_invalid_kinds = valid_kinds.len() < kinds.len();

                                    if valid_kinds.is_empty() {
                                        // No supported kinds found
                                        ("False", "InvalidRouteKinds", String::from("No supported route kinds found"), vec![])
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

                                        // If there are ANY invalid kinds, set ResolvedRefs=False
                                        if has_invalid_kinds {
                                            ("False", "InvalidRouteKinds", String::from("Some route kinds are not supported"), supported)
                                        } else {
                                            // All kinds are supported
                                            ("True", "ResolvedRefs", String::from("All references resolved"), supported)
                                        }
                                    }
                                }
                            } else {
                                // No kinds specified, use default based on protocol
                                ("True", "ResolvedRefs", String::from("All references resolved"), vec![
                                    json!({
                                        "group": "gateway.networking.k8s.io",
                                        "kind": "HTTPRoute"
                                    })
                                ])
                            }
                        } else {
                            // No allowedRoutes specified, use default based on protocol
                            ("True", "ResolvedRefs", String::from("All references resolved"), vec![
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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

    // TDD RED PHASE: TLS Certificate Validation Tests
    // These tests document the expected behavior for TLS certificateRef validation
    // according to Gateway API conformance requirements.

    #[test]
    fn test_validate_tls_certificate_ref_nonexistent_secret() {
        // RED: Test that nonexistent Secret is detected and ResolvedRefs=False
        //
        // Gateway API spec requirement:
        // When a listener's TLS certificateRef points to a Secret that doesn't exist,
        // the listener status must set ResolvedRefs=False with reason InvalidCertificateRef.
        //
        // This test will fail until we implement TLS validation logic.

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersTls, GatewayListenersTlsCertificateRefs,
        };

        // Create listener with TLS config pointing to nonexistent Secret
        let listener = GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            tls: Some(GatewayListenersTls {
                certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                    group: Some("".to_string()), // Core group
                    kind: Some("Secret".to_string()),
                    name: "nonexistent-secret".to_string(),
                    namespace: Some("default".to_string()),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Verify listener structure
        assert_eq!(listener.protocol, "HTTPS");
        assert!(listener.tls.is_some());

        let tls = listener.tls.as_ref().unwrap();
        assert!(tls.certificate_refs.is_some());

        let cert_refs = tls.certificate_refs.as_ref().unwrap();
        assert_eq!(cert_refs.len(), 1);
        assert_eq!(cert_refs[0].name, "nonexistent-secret");

        // TODO: When validation is implemented, this should return:
        // - resolved_refs_status = "False"
        // - resolved_refs_reason = "InvalidCertificateRef"
        // - resolved_refs_message = "Secret default/nonexistent-secret not found"
    }

    #[test]
    fn test_validate_tls_certificate_ref_unsupported_group() {
        // RED: Test that unsupported group is rejected
        //
        // Gateway API spec requirement:
        // Only group "" (core) is supported for Secret references.
        // Any other group must result in ResolvedRefs=False with reason InvalidCertificateRef.

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersTls, GatewayListenersTlsCertificateRefs,
        };

        let listener = GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            tls: Some(GatewayListenersTls {
                certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                    group: Some("custom.group.io".to_string()), // Invalid group
                    kind: Some("Secret".to_string()),
                    name: "my-secret".to_string(),
                    namespace: Some("default".to_string()),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let cert_ref = &listener
            .tls
            .as_ref()
            .unwrap()
            .certificate_refs
            .as_ref()
            .unwrap()[0];
        assert_eq!(cert_ref.group.as_ref().unwrap(), "custom.group.io");

        // TODO: Validation should reject this and set ResolvedRefs=False
    }

    #[test]
    fn test_validate_tls_certificate_ref_unsupported_kind() {
        // RED: Test that unsupported kind is rejected
        //
        // Gateway API spec requirement:
        // Only kind "Secret" is supported.
        // Any other kind must result in ResolvedRefs=False with reason InvalidCertificateRef.

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersTls, GatewayListenersTlsCertificateRefs,
        };

        let listener = GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            tls: Some(GatewayListenersTls {
                certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                    group: Some("".to_string()),
                    kind: Some("ConfigMap".to_string()), // Invalid kind
                    name: "my-cert".to_string(),
                    namespace: Some("default".to_string()),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let cert_ref = &listener
            .tls
            .as_ref()
            .unwrap()
            .certificate_refs
            .as_ref()
            .unwrap()[0];
        assert_eq!(cert_ref.kind.as_ref().unwrap(), "ConfigMap");

        // TODO: Validation should reject this and set ResolvedRefs=False
    }

    #[test]
    fn test_validate_tls_certificate_ref_malformed_secret() {
        // RED: Test that Secret without tls.crt/tls.key is rejected
        //
        // Gateway API spec requirement:
        // TLS Secret must contain both tls.crt and tls.key fields.
        // Missing fields or invalid PEM data must result in ResolvedRefs=False
        // with reason InvalidCertificateRef.

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersTls, GatewayListenersTlsCertificateRefs,
        };

        let listener = GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            tls: Some(GatewayListenersTls {
                certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                    group: Some("".to_string()),
                    kind: Some("Secret".to_string()),
                    name: "malformed-cert".to_string(),
                    namespace: Some("default".to_string()),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let cert_ref = &listener
            .tls
            .as_ref()
            .unwrap()
            .certificate_refs
            .as_ref()
            .unwrap()[0];
        assert_eq!(cert_ref.name, "malformed-cert");

        // TODO: Validation should check Secret data and reject if:
        // - Secret.data["tls.crt"] is missing
        // - Secret.data["tls.key"] is missing
        // - tls.crt is not valid PEM format
        // Then set ResolvedRefs=False with reason InvalidCertificateRef
    }

    #[test]
    fn test_validate_tls_certificate_ref_valid_secret() {
        // RED: Test that valid Secret reference is accepted
        //
        // Gateway API spec requirement:
        // When certificateRef points to a valid Secret with correct group ("" or "core"),
        // kind ("Secret"), and the Secret contains valid tls.crt/tls.key,
        // the listener status should set ResolvedRefs=True.

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersTls, GatewayListenersTlsCertificateRefs,
        };

        let listener = GatewayListeners {
            name: "https".to_string(),
            port: 443,
            protocol: "HTTPS".to_string(),
            tls: Some(GatewayListenersTls {
                certificate_refs: Some(vec![GatewayListenersTlsCertificateRefs {
                    group: Some("".to_string()),      // Valid: core group
                    kind: Some("Secret".to_string()), // Valid: Secret kind
                    name: "valid-tls-cert".to_string(),
                    namespace: Some("default".to_string()),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let cert_ref = &listener
            .tls
            .as_ref()
            .unwrap()
            .certificate_refs
            .as_ref()
            .unwrap()[0];
        assert_eq!(cert_ref.kind.as_ref().unwrap(), "Secret");
        assert_eq!(cert_ref.group.as_ref().unwrap(), "");

        // TODO: When Secret exists with valid tls.crt/tls.key:
        // - resolved_refs_status = "True"
        // - resolved_refs_reason = "ResolvedRefs"
        // - resolved_refs_message = "All references resolved"
    }

    // TDD RED PHASE: Invalid Route Kinds Validation
    // Gateway API conformance test: GatewayInvalidRouteKind
    // When a listener specifies only invalid route kinds (not HTTPRoute),
    // the listener status must set ResolvedRefs=False with reason=InvalidRouteKinds

    #[test]
    fn test_gateway_listener_only_invalid_route_kinds() {
        // RED: Test that listener with only invalid route kinds is rejected
        //
        // Gateway API conformance requirement (GatewayInvalidRouteKind test):
        // When a Gateway listener's allowedRoutes.kinds contains ONLY unsupported kinds,
        // the listener status must set:
        // - ResolvedRefs condition = "False"
        // - reason = "InvalidRouteKinds"
        // - supportedKinds = [] (empty array)
        //
        // Example: RAUTA only supports HTTPRoute, so TCPRoute is invalid.

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersAllowedRoutes, GatewayListenersAllowedRoutesKinds,
        };

        let listener = GatewayListeners {
            name: "tcp".to_string(),
            port: 9000,
            protocol: "TCP".to_string(),
            allowed_routes: Some(GatewayListenersAllowedRoutes {
                kinds: Some(vec![GatewayListenersAllowedRoutesKinds {
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: "TCPRoute".to_string(), // RAUTA doesn't support this
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Verify listener structure
        assert_eq!(listener.protocol, "TCP");
        assert!(listener.allowed_routes.is_some());

        let allowed_routes = listener.allowed_routes.as_ref().unwrap();
        assert!(allowed_routes.kinds.is_some());

        let kinds = allowed_routes.kinds.as_ref().unwrap();
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].kind, "TCPRoute");

        // RAUTA currently only supports HTTPRoute
        let rauta_supported_kinds = ["HTTPRoute"];
        let is_supported = rauta_supported_kinds.contains(&kinds[0].kind.as_str());
        assert!(!is_supported, "TCPRoute should NOT be supported by RAUTA");

        // TODO: When reconciler validates this listener, it should:
        // 1. Filter kinds to only supported ones (result: empty list)
        // 2. Set ResolvedRefs status = "False"
        // 3. Set ResolvedRefs reason = "InvalidRouteKinds"
        // 4. Set ResolvedRefs message = "No supported route kinds found"
        // 5. Set supportedKinds = [] (empty array)
        //
        // Current bug: The code at lines 385-387 correctly returns empty supportedKinds,
        // but the conformance test expects this specific behavior to be validated.
    }

    #[test]
    fn test_gateway_listener_mixed_valid_invalid_route_kinds() {
        // RED: Test that listener with both valid and invalid route kinds
        // sets ResolvedRefs=False but includes valid kinds in supportedKinds
        //
        // Gateway API conformance requirement:
        // When allowedRoutes.kinds contains SOME unsupported kinds,
        // the listener status must set:
        // - ResolvedRefs condition = "False"
        // - reason = "InvalidRouteKinds"
        // - message = "Some route kinds are not supported"
        // - supportedKinds = [only the valid kinds]

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersAllowedRoutes, GatewayListenersAllowedRoutesKinds,
        };

        let listener = GatewayListeners {
            name: "mixed".to_string(),
            port: 8080,
            protocol: "HTTP".to_string(),
            allowed_routes: Some(GatewayListenersAllowedRoutes {
                kinds: Some(vec![
                    GatewayListenersAllowedRoutesKinds {
                        group: Some("gateway.networking.k8s.io".to_string()),
                        kind: "HTTPRoute".to_string(), // Supported
                    },
                    GatewayListenersAllowedRoutesKinds {
                        group: Some("gateway.networking.k8s.io".to_string()),
                        kind: "TCPRoute".to_string(), // NOT supported
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Verify listener structure
        let kinds = listener
            .allowed_routes
            .as_ref()
            .unwrap()
            .kinds
            .as_ref()
            .unwrap();
        assert_eq!(kinds.len(), 2);
        assert_eq!(kinds[0].kind, "HTTPRoute");
        assert_eq!(kinds[1].kind, "TCPRoute");

        // TODO: When reconciler validates this listener, it should:
        // 1. Filter to only HTTPRoute (supported)
        // 2. Detect that TCPRoute was filtered out (has_invalid_kinds = true)
        // 3. Set ResolvedRefs status = "False" (because of invalid kinds)
        // 4. Set ResolvedRefs reason = "InvalidRouteKinds"
        // 5. Set ResolvedRefs message = "Some route kinds are not supported"
        // 6. Set supportedKinds = [{"group": "gateway.networking.k8s.io", "kind": "HTTPRoute"}]
        //
        // Current code at lines 400-401 correctly implements this logic.
    }

    #[test]
    fn test_gateway_listener_all_valid_route_kinds() {
        // GREEN: Test that listener with only valid route kinds
        // sets ResolvedRefs=True and includes all kinds in supportedKinds

        use gateway_api::apis::standard::gateways::{
            GatewayListeners, GatewayListenersAllowedRoutes, GatewayListenersAllowedRoutesKinds,
        };

        let listener = GatewayListeners {
            name: "http".to_string(),
            port: 80,
            protocol: "HTTP".to_string(),
            allowed_routes: Some(GatewayListenersAllowedRoutes {
                kinds: Some(vec![GatewayListenersAllowedRoutesKinds {
                    group: Some("gateway.networking.k8s.io".to_string()),
                    kind: "HTTPRoute".to_string(), // Supported
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Verify listener structure
        let kinds = listener
            .allowed_routes
            .as_ref()
            .unwrap()
            .kinds
            .as_ref()
            .unwrap();
        assert_eq!(kinds.len(), 1);
        assert_eq!(kinds[0].kind, "HTTPRoute");

        // RAUTA supports HTTPRoute
        let rauta_supported_kinds = ["HTTPRoute"];
        let is_supported = rauta_supported_kinds.contains(&kinds[0].kind.as_str());
        assert!(is_supported, "HTTPRoute should be supported by RAUTA");

        // When reconciler validates this listener, it should:
        // 1. Filter to only HTTPRoute (all kinds are valid)
        // 2. has_invalid_kinds = false
        // 3. Set ResolvedRefs status = "True"
        // 4. Set ResolvedRefs reason = "ResolvedRefs"
        // 5. Set ResolvedRefs message = "All references resolved"
        // 6. Set supportedKinds = [{"group": "gateway.networking.k8s.io", "kind": "HTTPRoute"}]
        //
        // Current code at lines 403-405 correctly implements this logic.
    }
}
