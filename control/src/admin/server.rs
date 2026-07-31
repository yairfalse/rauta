//! Admin HTTP Server
//!
//! Serves management endpoints on port 9091 (configurable via RAUTA_ADMIN_PORT).
//! Separate from the proxy port (8080) — management traffic never competes with data plane.

use crate::admin::local_query::LocalGatewayQuery;
use agent_api::query::GatewayQuery;
use agent_api::temporal::TemporalQuery;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

/// Admin server that serves management REST API
pub struct AdminServer {
    query: Arc<LocalGatewayQuery>,
    bind_addr: SocketAddr,
}

impl AdminServer {
    pub fn new(query: LocalGatewayQuery, bind_addr: SocketAddr) -> Self {
        Self {
            query: Arc::new(query),
            bind_addr,
        }
    }

    /// Start the admin server (runs until cancelled)
    pub async fn serve(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.bind_addr).await?;
        info!("Admin server listening on {}", self.bind_addr);

        loop {
            let (stream, _remote) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let query = Arc::clone(&self.query);

            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let query = Arc::clone(&query);
                    async move { handle_admin_request(req, query).await }
                });

                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    error!("Admin connection error: {}", e);
                }
            });
        }
    }
}

async fn handle_admin_request(
    req: Request<hyper::body::Incoming>,
    query: Arc<LocalGatewayQuery>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let response = match (method.as_str(), path.as_str()) {
        ("GET", "/api/v1/status") => handle_status(&query).await,
        ("GET", "/api/v1/routes") => handle_list_routes(&query).await,
        ("GET", "/api/v1/cache") => handle_cache_stats(&query).await,
        ("GET", "/api/v1/circuit-breakers") => handle_circuit_breakers(&query).await,
        ("GET", "/api/v1/rate-limiters") => handle_rate_limiters(&query).await,
        ("GET", "/api/v1/listeners") => handle_listeners(&query).await,
        ("GET", "/api/v1/metrics") => handle_metrics(&query).await,
        ("GET", "/api/v1/timeline") => handle_timeline(&query, req.uri().query()).await,
        ("GET", "/api/v1/diff") => handle_diff(&query, req.uri().query()).await,
        ("POST", "/api/v1/backends/drain") => unavailable_response(
            "backend_drain_unavailable",
            "Backend drain is not yet available through the admin API",
        ),
        ("POST", "/api/v1/backends/undrain") => unavailable_response(
            "backend_undrain_unavailable",
            "Backend undrain is not yet available through the admin API",
        ),
        ("POST", "/api/v1/diagnose") => {
            // Read symptom from query string or body
            let query_string = req.uri().query();
            let symptom = query_string
                .and_then(|q| query_value(q, "symptom"))
                .unwrap_or_else(|| "degraded".to_string());
            let since_seconds = query_string.and_then(query_u64("since_seconds"));

            handle_diagnose(&query, &symptom, since_seconds).await
        }
        ("GET", "/healthz") => json_response(StatusCode::OK, r#"{"status":"ok"}"#),
        _ => json_response(StatusCode::NOT_FOUND, r#"{"error":"not found"}"#),
    };

    Ok(response)
}

async fn handle_status(query: &LocalGatewayQuery) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.snapshot().await)
}

async fn handle_list_routes(query: &LocalGatewayQuery) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.list_routes(None, None).await)
}

async fn handle_cache_stats(query: &LocalGatewayQuery) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.cache_stats().await)
}

async fn handle_circuit_breakers(
    query: &LocalGatewayQuery,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.list_circuit_breakers(None).await)
}

async fn handle_rate_limiters(query: &LocalGatewayQuery) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.list_rate_limiters(None).await)
}

async fn handle_listeners(query: &LocalGatewayQuery) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.list_listeners().await)
}

async fn handle_metrics(query: &LocalGatewayQuery) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.metrics_snapshot(None).await)
}

async fn handle_timeline(
    query: &LocalGatewayQuery,
    query_string: Option<&str>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.timeline(temporal_query(query_string)).await)
}

async fn handle_diff(
    query: &LocalGatewayQuery,
    query_string: Option<&str>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(query.diff(temporal_query(query_string)).await)
}

async fn handle_diagnose(
    query: &LocalGatewayQuery,
    symptom: &str,
    since_seconds: Option<u64>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    query_response(
        query
            .diagnose_since(symptom, None, None, since_seconds)
            .await,
    )
}

fn temporal_query(query: Option<&str>) -> TemporalQuery {
    TemporalQuery {
        since_seconds: query.and_then(query_u64("since_seconds")),
        from_sequence: query.and_then(query_u64("from_sequence")),
        to_sequence: query.and_then(query_u64("to_sequence")),
    }
}

fn query_u64(key: &'static str) -> impl FnOnce(&str) -> Option<u64> {
    move |query| query_value(query, key).and_then(|value| value.parse::<u64>().ok())
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        (candidate == key).then(|| value.to_string())
    })
}

fn query_response<T: Serialize>(
    result: anyhow::Result<T>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    match result {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(json) => json_response(StatusCode::OK, &json),
            Err(e) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "serialization_failed",
                &format!("serialization failed: {}", e),
            ),
        },
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "query_failed",
            &e.to_string(),
        ),
    }
}

fn unavailable_response(code: &str, message: &str) -> Response<BoxBody<Bytes, hyper::Error>> {
    error_response(StatusCode::NOT_IMPLEMENTED, code, message)
}

fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    json_response(
        status,
        &serde_json::json!({
            "error": {
                "code": code,
                "message": message
            }
        })
        .to_string(),
    )
}

#[allow(clippy::unwrap_used)]
fn json_response(status: StatusCode, body: &str) -> Response<BoxBody<Bytes, hyper::Error>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(
            Full::new(Bytes::from(body.to_string()))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}
