//! Dual-protocol backend server for load testing
//!
//! Supports both HTTP/1.1 and HTTP/2 (auto-negotiation)

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use hyper_util::server::conn::auto;
use std::net::SocketAddr;
use tokio::net::TcpListener;

async fn handle_request(
    _req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, String> {
    let version = std::env::var("BACKEND_VERSION").unwrap_or_else(|_| "v1".to_string());

    let body = format!(
        r#"{{"status":"ok","version":"{}","backend":"rust-http2"}}"#,
        version
    );

    let response = Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| format!("Failed to build response: {}", e))?;

    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get port from environment or use default
    let port = std::env::var("BACKEND_PORT")
        .unwrap_or_else(|_| "8082".to_string())
        .parse::<u16>()
        .unwrap_or(8082);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;

    println!("Backend server listening on http://{}", addr);
    println!("Supports HTTP/1.1 and HTTP/2 (auto-negotiation)");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::spawn(async move {
            // Use auto-negotiation (supports both HTTP/1.1 and HTTP/2)
            if let Err(e) = auto::Builder::new(TokioExecutor::new())
                .serve_connection(io, service_fn(handle_request))
                .await
            {
                eprintln!("Error serving connection: {}", e);
            }
        });
    }
}
