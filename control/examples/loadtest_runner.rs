//! Load test runner for HTTP/2 backend pool
//!
//! Starts RAUTA proxy with route to HTTP/2 backend

use common::{Backend, HttpMethod};
use control::proxy::router::Router;
use control::proxy::server::ProxyServer;
use std::net::Ipv4Addr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), String> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("info,control=debug")
        .init();

    println!("🚀 Starting RAUTA Load Test");
    println!("================================");

    // Create router
    let router = Arc::new(Router::new());

    // Add route to HTTP/2 backend (127.0.0.1:9001)
    let backend = Backend::new(u32::from(Ipv4Addr::new(127, 0, 0, 1)), 9001, 100);

    router
        .add_route(HttpMethod::GET, "/api/test", vec![backend])
        .map_err(|e| format!("Failed to add route: {}", e))?;

    println!("✅ Route configured: GET /api/test → 127.0.0.1:9001");

    // Create proxy server
    let server = ProxyServer::new("127.0.0.1:8081".to_string(), router)
        .map_err(|e| format!("Failed to create server: {}", e))?;

    // Mark backend as HTTP/2
    server.set_backend_protocol_http2("127.0.0.1", 9001).await;
    println!("✅ Backend marked as HTTP/2");

    println!("\n🌐 RAUTA Proxy listening on http://127.0.0.1:8081");
    println!("📊 Metrics available at http://127.0.0.1:8081/metrics");
    println!("\n📍 Test endpoint: http://127.0.0.1:8081/api/test");
    println!("\n🔥 Ready for load test:");
    println!("   wrk -t4 -c400 -d30s http://127.0.0.1:8081/api/test\n");

    // Serve
    server.serve().await
}
