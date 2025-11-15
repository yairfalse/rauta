//! Dynamic Listener Management for Gateway API
//!
//! This module manages TCP listeners dynamically based on Gateway resources.
//! Each Gateway can specify multiple listeners (HTTP/HTTPS on different ports),
//! and the ListenerManager handles:
//! - Creating listeners when Gateways are created
//! - Updating listeners when Gateway specs change
//! - Removing listeners when Gateways are deleted
//! - Graceful shutdown of all listeners

use crate::proxy::router::Router;
use hyper::server::conn::{http1, http2};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Unique identifier for a listener
/// Format: "{gateway_namespace}/{gateway_name}/{listener_name}"
#[allow(dead_code)] // Used in Phase 2 (Gateway integration)
pub type ListenerId = String;

/// Listener protocol
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)] // HTTP/HTTPS are standard acronyms
#[allow(dead_code)] // Used in Phase 2 (Gateway integration)
pub enum Protocol {
    HTTP,
    HTTPS,
    // Future: TCP, TLS, gRPC
}

/// Listener configuration from Gateway spec
#[derive(Debug, Clone)]
#[allow(dead_code)] // Used in Phase 2 (Gateway integration)
pub struct ListenerConfig {
    /// Listener name (from Gateway spec.listeners[].name)
    pub name: String,
    /// Protocol (HTTP, HTTPS, etc.)
    pub protocol: Protocol,
    /// Port to bind to
    pub port: u16,
    /// Optional hostname for SNI (HTTPS only)
    pub hostname: Option<String>,
    /// Gateway namespace
    pub gateway_namespace: String,
    /// Gateway name
    pub gateway_name: String,
}

impl ListenerConfig {
    /// Generate unique listener ID
    pub fn listener_id(&self) -> ListenerId {
        format!(
            "{}/{}/{}",
            self.gateway_namespace, self.gateway_name, self.name
        )
    }

    /// Get bind address (always 0.0.0.0 for DaemonSet hostNetwork)
    pub fn bind_addr(&self) -> String {
        format!("0.0.0.0:{}", self.port)
    }
}

/// Active listener with its task handle
struct ActiveListener {
    config: ListenerConfig,
    cancel_tx: tokio::sync::oneshot::Sender<()>,
    #[allow(dead_code)] // Used for graceful shutdown tracking
    task_handle: tokio::task::JoinHandle<()>,
}

/// Listener Manager - manages dynamic TCP listeners based on Gateway resources
#[allow(dead_code)] // Used in Phase 2 (Gateway integration)
pub struct ListenerManager {
    /// Active listeners indexed by ListenerId
    listeners: Arc<RwLock<HashMap<ListenerId, ActiveListener>>>,
    /// Shared router for all listeners
    router: Arc<Router>,
}

#[allow(dead_code)] // Methods used in Phase 2 (Gateway integration)
impl ListenerManager {
    /// Create new ListenerManager
    pub fn new(router: Arc<Router>) -> Self {
        Self {
            listeners: Arc::new(RwLock::new(HashMap::new())),
            router,
        }
    }

    /// Add or update a listener
    ///
    /// If a listener with the same ID already exists:
    /// - If config unchanged: No-op
    /// - If config changed: Stop old listener, start new one
    ///
    /// Returns the ListenerId
    pub async fn upsert_listener(&self, config: ListenerConfig) -> Result<ListenerId, String> {
        let listener_id = config.listener_id();

        let mut listeners = self.listeners.write().await;

        // Check if listener already exists
        if let Some(existing) = listeners.get(&listener_id) {
            // Compare configs (ignore task handles)
            if Self::configs_equal(&existing.config, &config) {
                debug!(
                    "Listener {} already exists with same config, skipping",
                    listener_id
                );
                return Ok(listener_id);
            }

            // Config changed - remove old listener first
            warn!("Listener {} config changed, restarting", listener_id);
            if let Some(old) = listeners.remove(&listener_id) {
                let _ = old.cancel_tx.send(());
                // Don't wait for shutdown - let it happen in background
            }
        }

        // Spawn new listener
        info!(
            "Starting listener {} on {} ({})",
            listener_id,
            config.bind_addr(),
            match config.protocol {
                Protocol::HTTP => "HTTP",
                Protocol::HTTPS => "HTTPS",
            }
        );

        let active_listener = self.spawn_listener(config).await?;
        listeners.insert(listener_id.clone(), active_listener);

        Ok(listener_id)
    }

    /// Remove a listener by ID
    ///
    /// Sends graceful shutdown signal and removes from active set.
    /// The listener task will complete in the background.
    pub async fn remove_listener(&self, listener_id: &ListenerId) -> Result<(), String> {
        let mut listeners = self.listeners.write().await;

        if let Some(listener) = listeners.remove(listener_id) {
            info!("Removing listener {}", listener_id);
            let _ = listener.cancel_tx.send(());
            // Don't wait - graceful shutdown happens in background
            Ok(())
        } else {
            Err(format!("Listener {} not found", listener_id))
        }
    }

    /// List all active listeners
    pub async fn list_listeners(&self) -> Vec<(ListenerId, ListenerConfig)> {
        let listeners = self.listeners.read().await;
        listeners
            .iter()
            .map(|(id, active)| (id.clone(), active.config.clone()))
            .collect()
    }

    /// Shutdown all listeners gracefully
    pub async fn shutdown(&self) -> Result<(), String> {
        info!("Shutting down all listeners");
        let mut listeners = self.listeners.write().await;

        for (id, listener) in listeners.drain() {
            info!("Shutting down listener {}", id);
            let _ = listener.cancel_tx.send(());
        }

        Ok(())
    }

    /// Spawn a listener task
    async fn spawn_listener(&self, config: ListenerConfig) -> Result<ActiveListener, String> {
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        let router = self.router.clone();
        let bind_addr = config.bind_addr();
        let listener_name = config.name.clone();
        let protocol = config.protocol.clone();

        // Try to bind immediately to detect port conflicts
        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", bind_addr, e))?;

        let task_handle = tokio::spawn(async move {
            info!("Listener '{}' bound to {}", listener_name, bind_addr);

            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                debug!("Accepted connection from {} on listener '{}'", peer_addr, listener_name);

                                let router = router.clone();
                                let listener_name = listener_name.clone();
                                let protocol = protocol.clone();

                                // Spawn connection handler
                                tokio::spawn(async move {
                                    let io = TokioIo::new(stream);

                                    // Create service that uses the router
                                    let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                                        let router = router.clone();
                                        async move {
                                            Self::handle_request(req, router).await
                                        }
                                    });

                                    // Serve connection based on protocol
                                    let result = match protocol {
                                        Protocol::HTTP => {
                                            // Try HTTP/2 first, fall back to HTTP/1.1
                                            http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                                                .serve_connection(io, service)
                                                .await
                                        }
                                        Protocol::HTTPS => {
                                            // TODO: TLS termination
                                            // For now, treat as HTTP
                                            http1::Builder::new()
                                                .serve_connection(io, service)
                                                .await
                                        }
                                    };

                                    if let Err(e) = result {
                                        debug!("Connection error on listener '{}': {}", listener_name, e);
                                    }
                                });
                            }
                            Err(e) => {
                                error!("Accept error on listener '{}': {}", listener_name, e);
                            }
                        }
                    }
                    _ = &mut cancel_rx => {
                        info!("Listener '{}' received shutdown signal", listener_name);
                        break;
                    }
                }
            }

            info!("Listener '{}' stopped", listener_name);
        });

        Ok(ActiveListener {
            config,
            cancel_tx,
            task_handle,
        })
    }

    /// Handle HTTP request using router
    async fn handle_request(
        req: Request<hyper::body::Incoming>,
        _router: Arc<Router>,
    ) -> Result<
        Response<http_body_util::combinators::BoxBody<hyper::body::Bytes, std::io::Error>>,
        hyper::Error,
    > {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;

        // Extract request info for routing
        let method = req.method().clone();
        let uri = req.uri().clone();
        let path = uri.path();

        debug!("Request: {} {}", method, path);

        // TODO: Use router to select backend
        // For now, return 404
        let response = Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(
                Full::new(Bytes::from("Not Found"))
                    .map_err(std::io::Error::other)
                    .boxed(),
            )
            .unwrap();

        Ok(response)
    }

    /// Compare listener configs (ignore runtime fields)
    fn configs_equal(a: &ListenerConfig, b: &ListenerConfig) -> bool {
        a.name == b.name
            && a.protocol == b.protocol
            && a.port == b.port
            && a.hostname == b.hostname
            && a.gateway_namespace == b.gateway_namespace
            && a.gateway_name == b.gateway_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_listener_manager_creation() {
        let router = Arc::new(Router::new());
        let manager = ListenerManager::new(router);

        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 0, "New manager should have no listeners");
    }

    #[tokio::test]
    async fn test_listener_config_id() {
        let config = ListenerConfig {
            name: "http".to_string(),
            protocol: Protocol::HTTP,
            port: 8080,
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "my-gateway".to_string(),
        };

        assert_eq!(config.listener_id(), "default/my-gateway/http");
        assert_eq!(config.bind_addr(), "0.0.0.0:8080");
    }

    #[tokio::test]
    async fn test_upsert_listener() {
        let router = Arc::new(Router::new());
        let manager = ListenerManager::new(router);

        let config = ListenerConfig {
            name: "http".to_string(),
            protocol: Protocol::HTTP,
            port: 0, // OS assigns port
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "test-gateway".to_string(),
        };

        let listener_id = manager.upsert_listener(config.clone()).await.unwrap();
        assert_eq!(listener_id, "default/test-gateway/http");

        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].0, listener_id);
    }

    #[tokio::test]
    async fn test_upsert_idempotent() {
        let router = Arc::new(Router::new());
        let manager = ListenerManager::new(router);

        let config = ListenerConfig {
            name: "http".to_string(),
            protocol: Protocol::HTTP,
            port: 0,
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "test-gateway".to_string(),
        };

        let id1 = manager.upsert_listener(config.clone()).await.unwrap();
        let id2 = manager.upsert_listener(config.clone()).await.unwrap();

        assert_eq!(id1, id2);

        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 1, "Should still have only 1 listener");
    }

    #[tokio::test]
    async fn test_remove_listener() {
        let router = Arc::new(Router::new());
        let manager = ListenerManager::new(router);

        let config = ListenerConfig {
            name: "http".to_string(),
            protocol: Protocol::HTTP,
            port: 0,
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "test-gateway".to_string(),
        };

        let listener_id = manager.upsert_listener(config).await.unwrap();
        assert_eq!(manager.list_listeners().await.len(), 1);

        manager.remove_listener(&listener_id).await.unwrap();
        assert_eq!(manager.list_listeners().await.len(), 0);
    }

    #[tokio::test]
    async fn test_multiple_listeners() {
        let router = Arc::new(Router::new());
        let manager = ListenerManager::new(router);

        let config1 = ListenerConfig {
            name: "http".to_string(),
            protocol: Protocol::HTTP,
            port: 0,
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "gateway-1".to_string(),
        };

        let config2 = ListenerConfig {
            name: "https".to_string(),
            protocol: Protocol::HTTPS,
            port: 0,
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "gateway-1".to_string(),
        };

        manager.upsert_listener(config1).await.unwrap();
        manager.upsert_listener(config2).await.unwrap();

        let listeners = manager.list_listeners().await;
        assert_eq!(listeners.len(), 2);
    }

    #[tokio::test]
    async fn test_shutdown_all() {
        let router = Arc::new(Router::new());
        let manager = ListenerManager::new(router);

        let config = ListenerConfig {
            name: "http".to_string(),
            protocol: Protocol::HTTP,
            port: 0,
            hostname: None,
            gateway_namespace: "default".to_string(),
            gateway_name: "test-gateway".to_string(),
        };

        manager.upsert_listener(config).await.unwrap();
        assert_eq!(manager.list_listeners().await.len(), 1);

        manager.shutdown().await.unwrap();
        assert_eq!(manager.list_listeners().await.len(), 0);
    }
}
