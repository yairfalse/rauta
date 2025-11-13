//! GatewayClass watcher
//!
//! Watches GatewayClass resources and accepts those with our controllerName.

use crate::apis::metrics::record_gatewayclass_reconciliation;
use futures::StreamExt;
use gateway_api::apis::standard::gatewayclasses::GatewayClass;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, ResourceExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info};

/// Controller name for RAUTA Gateway API implementation
#[allow(dead_code)] // Used in K8s mode
pub const RAUTA_CONTROLLER_NAME: &str = "rauta.io/gateway-controller";

/// GatewayClass reconciler
#[allow(dead_code)] // Used in K8s mode
pub struct GatewayClassReconciler {
    client: Client,
}

#[allow(dead_code)] // Used in K8s mode
impl GatewayClassReconciler {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Check if a GatewayClass should be accepted by this controller
    pub fn should_accept(&self, controller_name: &str) -> bool {
        controller_name == RAUTA_CONTROLLER_NAME
    }

    /// Reconcile a single GatewayClass
    async fn reconcile(
        gateway_class: Arc<GatewayClass>,
        ctx: Arc<Self>,
    ) -> Result<Action, kube::Error> {
        let start = Instant::now();
        let name = gateway_class.name_any();
        let controller_name = &gateway_class.spec.controller_name;

        info!("Reconciling GatewayClass: {}", name);

        // Check if this GatewayClass is for our controller
        if ctx.should_accept(controller_name) {
            info!("GatewayClass {} has our controllerName, accepting", name);
            ctx.set_accepted_status(&name, true).await?;

            // Record metrics for accepted GatewayClass
            record_gatewayclass_reconciliation(&name, start.elapsed().as_secs_f64(), "success");
        } else {
            debug!(
                "GatewayClass {} has controllerName '{}', ignoring",
                name, controller_name
            );
            // Don't update status - not our GatewayClass
        }

        // Requeue after 5 minutes for periodic reconciliation
        Ok(Action::requeue(Duration::from_secs(300)))
    }

    /// Update GatewayClass status with Accepted condition
    async fn set_accepted_status(&self, name: &str, accepted: bool) -> Result<(), kube::Error> {
        let api: Api<GatewayClass> = Api::all(self.client.clone());

        let status = if accepted {
            json!({
                "status": {
                    "conditions": [{
                        "type": "Accepted",
                        "status": "True",
                        "reason": "Accepted",
                        "message": format!("GatewayClass is accepted by controller {}", RAUTA_CONTROLLER_NAME),
                        "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    }]
                }
            })
        } else {
            json!({
                "status": {
                    "conditions": [{
                        "type": "Accepted",
                        "status": "False",
                        "reason": "InvalidParameters",
                        "message": "GatewayClass configuration is invalid",
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
            "Updated GatewayClass {} status: accepted={}",
            name, accepted
        );
        Ok(())
    }

    /// Error handler for controller
    fn error_policy(_obj: Arc<GatewayClass>, error: &kube::Error, _ctx: Arc<Self>) -> Action {
        error!("GatewayClass reconciliation error: {:?}", error);
        // Retry after 1 minute on errors
        Action::requeue(Duration::from_secs(60))
    }

    /// Start the GatewayClass controller
    pub async fn run(self) -> Result<(), kube::Error> {
        let api: Api<GatewayClass> = Api::all(self.client.clone());
        let ctx = Arc::new(self);

        info!("Starting GatewayClass controller");

        Controller::new(api, WatcherConfig::default())
            .run(Self::reconcile, Self::error_policy, ctx)
            .for_each(|res| async move {
                match res {
                    Ok(o) => debug!("Reconciled GatewayClass: {:?}", o),
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
    use gateway_api::apis::standard::gatewayclasses::{GatewayClass, GatewayClassSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    #[test]
    fn test_controller_name_constant() {
        // Verify our controller name is set correctly
        assert_eq!(RAUTA_CONTROLLER_NAME, "rauta.io/gateway-controller");
    }

    #[test]
    fn test_gateway_class_with_rauta_controller() {
        // RED: This test documents the Gateway API structure

        // Create a GatewayClass with our controllerName
        let gateway_class = GatewayClass {
            metadata: ObjectMeta {
                name: Some("rauta".to_string()),
                ..Default::default()
            },
            spec: GatewayClassSpec {
                controller_name: RAUTA_CONTROLLER_NAME.to_string(),
                ..Default::default()
            },
            status: None,
        };

        // Verify it has our controller name
        assert_eq!(gateway_class.spec.controller_name, RAUTA_CONTROLLER_NAME);

        // Note: Full reconcile() testing planned
        // This is where we'll test that:
        // 1. The reconciler accepts this GatewayClass
        // 2. It sets status.conditions with Accepted=true
        // 3. It sets observedGeneration correctly
    }

    #[test]
    fn test_gateway_class_with_other_controller() {
        // Verify we can detect other controllers
        let gateway_class = GatewayClass {
            metadata: ObjectMeta {
                name: Some("nginx".to_string()),
                ..Default::default()
            },
            spec: GatewayClassSpec {
                controller_name: "nginx.org/controller".to_string(),
                ..Default::default()
            },
            status: None,
        };

        // Should NOT match our controller name
        assert_ne!(gateway_class.spec.controller_name, RAUTA_CONTROLLER_NAME);
    }

    #[test]
    fn test_gatewayclass_metrics_recorded() {
        // RED: Test that GatewayClass reconciliation records metrics
        use crate::apis::metrics::gather_controller_metrics;

        // Record a fake reconciliation
        crate::apis::metrics::record_gatewayclass_reconciliation("rauta", 0.021, "success");

        // Gather metrics and verify
        let metrics = gather_controller_metrics().expect("Should gather metrics");

        assert!(
            metrics.contains("gatewayclass_reconciliations_total"),
            "Should contain counter metric"
        );
    }
}
