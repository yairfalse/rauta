//! HTTP Server - proxies requests to backends
//!
//! TDD: Starting with tests

use crate::proxy::router::Router;
use http_body_util::Full;
use hyper::{body::Bytes, Request, Response};
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

    /// Handle incoming HTTP request
    pub async fn handle_request(
        &self,
        _req: Request<hyper::body::Body>,
    ) -> Result<Response<Full<Bytes>>, String> {
        // GREEN: Minimal implementation - just return 200 OK
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
        // RED: This test will FAIL - handle_request doesn't exist yet
        use http_body_util::Empty;
        use hyper::{body::Bytes, Request};

        let router = crate::proxy::router::Router::new();

        // Add route: GET /hello -> 127.0.0.1:9999
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router.add_route(HttpMethod::GET, "/hello", backends).unwrap();

        let server = ProxyServer::new("127.0.0.1:0".to_string(), router).unwrap();

        // Try to call handle_request (will fail - doesn't exist!)
        let req = Request::builder()
            .method("GET")
            .uri("/hello")
            .body(Empty::<Bytes>::new())
            .unwrap();

        let response = server.handle_request(req).await;

        // Should get a response
        assert!(response.is_ok());
    }
}
