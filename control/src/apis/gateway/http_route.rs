//! HTTPRoute watcher
//!
//! Watches HTTPRoute resources and updates routing rules.

use crate::proxy::router::Router;
use kube::Client;
use std::sync::Arc;

/// HTTPRoute reconciler
#[allow(dead_code)]
pub struct HTTPRouteReconciler {
    client: Client,
    router: Arc<Router>,
}

#[allow(dead_code)]
impl HTTPRouteReconciler {
    pub fn new(client: Client, router: Arc<Router>) -> Self {
        Self { client, router }
    }
}

// TODO: Implement HTTPRoute watcher (Phase 1)
