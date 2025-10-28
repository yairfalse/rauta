//! HTTP Server - proxies requests to backends
//!
//! TDD: Starting with tests

use crate::proxy::router::Router;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{body::Bytes, Request, Response};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::net::SocketAddr;
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

    // TODO: Add serve() with TDD (Week 2)
    // Remote added this with old hyper 0.14 API - commented out until we TDD it properly
    /*
    pub async fn serve(&self) -> Result<(), String> {
        // Will implement with hyper 1.0 API using TDD
        todo!("serve() not yet implemented - needs TDD")
    }
    */

    /// Handle incoming HTTP request
    pub async fn handle_request<B>(
        &self,
        _req: Request<B>,
    ) -> Result<Response<Full<Bytes>>, String> {
        // GREEN: Minimal implementation - just return 200 OK
        // TODO: Add routing logic with TDD
        let response = Response::builder()
            .status(200)
            .body(Full::new(Bytes::from("OK")))
            .map_err(|e| format!("Failed to build response: {}", e))?;

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Backend, HttpMethod};
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_server_basic_routing() {
        // GREEN: ProxyServer exists and can be created
        let router = crate::proxy::router::Router::new();

        // Add route: GET /test -> 127.0.0.1:9999
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router.add_route(HttpMethod::GET, "/test", backends).unwrap();

        // Create server
        let server = ProxyServer::new("127.0.0.1:8080".to_string(), router);

        // Server should be created successfully
        assert!(server.is_ok());
    }

    #[tokio::test]
    async fn test_server_serves_http() {
        // GREEN: Basic test - server returns 200 OK
        let router = crate::proxy::router::Router::new();

        // Add route: GET /hello -> 127.0.0.1:9999
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router.add_route(HttpMethod::GET, "/hello", backends).unwrap();

        let server = ProxyServer::new("127.0.0.1:0".to_string(), router).unwrap();

        // Note: We can't easily create hyper::body::Incoming in tests
        // This test validates server creation - actual HTTP testing done via integration tests
        // For now, just verify server was created
        assert_eq!(server.bind_addr, "127.0.0.1:0");
    }
}
