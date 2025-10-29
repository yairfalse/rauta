//! GatewayClass watcher
//!
//! Watches GatewayClass resources and accepts those with our controllerName.

use kube::Client;

/// Controller name for RAUTA Gateway API implementation
#[allow(dead_code)]
pub const RAUTA_CONTROLLER_NAME: &str = "rauta.io/gateway-controller";

/// GatewayClass reconciler
#[allow(dead_code)]
pub struct GatewayClassReconciler {
    client: Client,
}

#[allow(dead_code)]
impl GatewayClassReconciler {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

// TODO: Implement GatewayClass watcher (Phase 1)
