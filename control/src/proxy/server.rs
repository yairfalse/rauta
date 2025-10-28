//! HTTP Server - proxies requests to backends

use crate::proxy::router::Router;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio::net::TcpListener;

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

    /// Start serving HTTP requests
    ///
    /// Returns the actual bound address (useful when binding to port 0)
    pub async fn serve(self) -> Result<(), String> {
        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| format!("Failed to bind to {}: {}", self.bind_addr, e))?;

        let _actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get local addr: {}", e))?;

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| format!("Failed to accept connection: {}", e))?;

            let router = self.router.clone();

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    async move { handle_request(req, router).await }
                });

                let _ = http1::Builder::new().serve_connection(io, service).await;
            });
        }
    }
}

/// Handle incoming HTTP request
async fn handle_request(
    req: Request<hyper::body::Incoming>,
    router: Arc<Router>,
) -> Result<Response<Full<Bytes>>, String> {
    let method = match req.method() {
        &hyper::Method::GET => common::HttpMethod::GET,
        &hyper::Method::POST => common::HttpMethod::POST,
        &hyper::Method::PUT => common::HttpMethod::PUT,
        &hyper::Method::DELETE => common::HttpMethod::DELETE,
        &hyper::Method::HEAD => common::HttpMethod::HEAD,
        &hyper::Method::OPTIONS => common::HttpMethod::OPTIONS,
        &hyper::Method::PATCH => common::HttpMethod::PATCH,
        _ => common::HttpMethod::GET,
    };

    let path = req.uri().path();

    // Try to route request
    let backend = router.select_backend(method, path, None, None);

    match backend {
        Some(backend) => {
            // Route matched! Return response showing which backend
            let body = format!(
                "Route matched!\nMethod: {:?}\nPath: {}\nBackend: {}.{}.{}.{}:{}\n",
                method,
                path,
                (backend.ipv4 >> 24) & 0xFF,
                (backend.ipv4 >> 16) & 0xFF,
                (backend.ipv4 >> 8) & 0xFF,
                backend.ipv4 & 0xFF,
                backend.port
            );

            Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(body)))
                .unwrap())
        }
        None => {
            // No route matched - 404
            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("Not Found")))
                .unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Backend, HttpMethod};
    use http_body_util::BodyExt;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_server_creation() {
        let router = crate::proxy::router::Router::new();

        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(127, 0, 0, 1)),
            9999,
            100,
        )];
        router.add_route(HttpMethod::GET, "/test", backends).unwrap();

        let server = ProxyServer::new("127.0.0.1:8080".to_string(), router);
        assert!(server.is_ok());
    }

    #[tokio::test]
    async fn test_server_routes_request() {
        let router = crate::proxy::router::Router::new();

        // Add route: GET /api/users -> 10.0.1.1:8080
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];
        router.add_route(HttpMethod::GET, "/api/users", backends).unwrap();

        // Bind to a specific available port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = ProxyServer::new(bind_addr.to_string(), router).unwrap();

        // Start server in background
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        // Wait for server to start
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make HTTP request
        let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/users", bind_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should succeed");

        // Server should return 200 OK (matched route)
        assert_eq!(response.status(), hyper::StatusCode::OK);

        // Response should indicate which backend was selected
        let body_bytes = response.collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(body.contains("10.0.1.1"), "Response should show backend IP");
    }

    #[tokio::test]
    async fn test_server_returns_404_for_no_match() {
        let router = crate::proxy::router::Router::new();

        // Add route: GET /api/users -> backend
        let backends = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];
        router.add_route(HttpMethod::GET, "/api/users", backends).unwrap();

        // Bind to a specific available port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind_addr = listener.local_addr().unwrap();
        drop(listener);

        let server = ProxyServer::new(bind_addr.to_string(), router).unwrap();

        // Start server in background
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Make request to non-existent route
        let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http::<Full<Bytes>>();

        let uri = format!("http://{}/api/posts", bind_addr).parse().unwrap();
        let response = client.get(uri).await.expect("Request should succeed");

        // Should return 404
        assert_eq!(response.status(), hyper::StatusCode::NOT_FOUND);
    }
}
