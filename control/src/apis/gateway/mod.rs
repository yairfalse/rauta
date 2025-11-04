//! Gateway API support (v1)
//!
//! Implements Kubernetes Gateway API watchers:
//! - GatewayClass: Controller identity and configuration
//! - Gateway: Infrastructure configuration (listeners, TLS)
//! - HTTPRoute: Routing rules (path, headers, backends)
//! - SecretWatcher: TLS certificate hot-reload

#[allow(clippy::module_inception)]
pub mod gateway;
pub mod gateway_class;
pub mod http_route;
pub mod secret_watcher;
