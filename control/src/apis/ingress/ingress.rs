//! Ingress watcher
//!
//! Watches Ingress resources and translates them to routing rules.

use crate::proxy::router::Router;
use kube::Client;
use std::sync::Arc;

/// Ingress reconciler
#[allow(dead_code)]
pub struct IngressReconciler {
    client: Client,
    router: Arc<Router>,
}

#[allow(dead_code)]
impl IngressReconciler {
    pub fn new(client: Client, router: Arc<Router>) -> Self {
        Self { client, router }
    }
}

// TODO: Implement Ingress watcher (Phase 2 - after Gateway API)
