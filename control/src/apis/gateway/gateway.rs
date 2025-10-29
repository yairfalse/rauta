//! Gateway watcher
//!
//! Watches Gateway resources and configures listeners (HTTP, HTTPS, TCP).

use kube::Client;

/// Gateway reconciler
#[allow(dead_code)]
pub struct GatewayReconciler {
    client: Client,
}

#[allow(dead_code)]
impl GatewayReconciler {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

// TODO: Implement Gateway watcher (Phase 1)
