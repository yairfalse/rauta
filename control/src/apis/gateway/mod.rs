//! Gateway API support (v1)
//!
//! Implements Kubernetes Gateway API watchers:
//! - GatewayClass: Controller identity and configuration
//! - Gateway: Infrastructure configuration (listeners, TLS)
//! - HTTPRoute: Routing rules (path, headers, backends)
//! - SecretWatcher: TLS certificate hot-reload
//! - EndpointSliceWatcher: Dynamic backend discovery
//! - GatewayIndex: Shared state for Gateway→GatewayClass mappings

pub mod endpointslice_watcher;
#[allow(clippy::module_inception)]
pub mod gateway;
pub mod gateway_class;
pub mod gateway_index;
pub mod http_route;
pub mod secret_watcher;
