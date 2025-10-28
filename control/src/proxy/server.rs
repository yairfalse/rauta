//! HTTP Server - proxies requests to backends
//!
//! TDD: Starting with tests

use crate::proxy::router::Router;
use std::sync::Arc;

/// HTTP Proxy Server
pub struct ProxyServer {
    bind_addr: String,
    router: Arc<Router>,
}

impl ProxyServer {
    /// Create new proxy server
    pub fn new(bind_addr: String, router: Router) -> Result<Self, String> {
        Ok(Self {
            bind_addr,
            router: Arc::new(router),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Backend, HttpMethod};
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_server_basic_routing() {
        // RED: This test will FAIL - ProxyServer doesn't exist yet
        let router = crate::proxy::router::Router::new();

        // Add route: GET /test -> 127.0.0.1:9999
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router.add_route(HttpMethod::GET, "/test", backends).unwrap();

        // Create server (will fail - doesn't exist yet!)
        let server = ProxyServer::new("127.0.0.1:8080".to_string(), router);

        // Server should be created successfully
        assert!(server.is_ok());
    }
}
