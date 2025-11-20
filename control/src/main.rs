use anyhow::Result;
use common::{Backend, HttpMethod};
use proxy::router::Router;
use proxy::server::ProxyServer;
use std::env;
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::signal;
use tracing::{info, warn};

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod apis;
mod config;
mod error;
mod proxy;

use apis::gateway::endpointslice_watcher::watch_endpointslices;
use apis::gateway::gateway::GatewayReconciler;
use apis::gateway::gateway_class::GatewayClassReconciler;
use apis::gateway::http_route::HTTPRouteReconciler;
use proxy::listener_manager::ListenerManager;

/// RAUTA Control Plane
///
/// Pure Rust userspace HTTP proxy with Gateway API support
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize rustls crypto provider (needed for Kubernetes TLS client)
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // Ignore error if already installed

    tracing_subscriber::fmt::init();

    info!("🦀 RAUTA Stage 1: Pure Rust Ingress Controller");

    // Check if running in Kubernetes mode
    let k8s_mode = env::var("RAUTA_K8S_MODE")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    // Create shared router (Arc for sharing with controllers)
    let router = Arc::new(Router::new());

    // Create listener manager for Gateway API dynamic listeners
    let listener_manager = Arc::new(ListenerManager::new(router.clone()));

    // Kubernetes Gateway API controllers (optional)
    let mut controller_handles = vec![];

    if k8s_mode {
        info!("🌐 Kubernetes mode enabled - starting Gateway API controllers");

        match kube::Client::try_default().await {
            Ok(client) => {
                let gateway_class_name =
                    env::var("RAUTA_GATEWAY_CLASS").unwrap_or_else(|_| "rauta".to_string());
                let gateway_name =
                    env::var("RAUTA_GATEWAY_NAME").unwrap_or_else(|_| "rauta-gateway".to_string());

                info!("   GatewayClass: {}", gateway_class_name);
                info!("   Gateway: {}", gateway_name);

                // Spawn GatewayClass controller
                let gc_client = client.clone();
                let gc_reconciler = GatewayClassReconciler::new(gc_client);
                controller_handles.push(tokio::spawn(async move {
                    if let Err(e) = gc_reconciler.run().await {
                        tracing::error!("GatewayClass controller error: {}", e);
                    }
                }));

                // Spawn Gateway controller
                let gw_client = client.clone();
                let gw_name = gateway_class_name.clone();
                let gw_listener_manager = listener_manager.clone();
                let gw_reconciler = GatewayReconciler::new(gw_client, gw_name, gw_listener_manager);
                controller_handles.push(tokio::spawn(async move {
                    if let Err(e) = gw_reconciler.run().await {
                        tracing::error!("Gateway controller error: {}", e);
                    }
                }));

                // Spawn HTTPRoute controller (with Router integration)
                let hr_client = client.clone();
                let hr_router = router.clone();
                let hr_reconciler = HTTPRouteReconciler::new(hr_client, hr_router, gateway_name);
                controller_handles.push(tokio::spawn(async move {
                    if let Err(e) = hr_reconciler.run().await {
                        tracing::error!("HTTPRoute controller error: {}", e);
                    }
                }));

                // Spawn EndpointSlice watcher (dynamic backend discovery)
                let es_client = client.clone();
                let es_router = router.clone();
                controller_handles.push(tokio::spawn(async move {
                    if let Err(e) = watch_endpointslices(es_client, es_router).await {
                        tracing::error!("EndpointSlice watcher error: {}", e);
                    }
                }));

                info!("✅ Gateway API controllers started (including EndpointSlice watcher)");
            }
            Err(e) => {
                warn!("Failed to create Kubernetes client: {}", e);
                warn!("Running in standalone mode with example routes");
                add_example_routes(&router)?;
            }
        }
    } else {
        info!("📝 Standalone mode - using example routes");
        add_example_routes(&router)?;
    }

    // In K8s mode, listeners are created dynamically by ListenerManager
    // In standalone mode, create a static ProxyServer
    if k8s_mode {
        info!("🎯 K8s mode: Listeners will be created dynamically by Gateway reconciliation");
        info!("Press Ctrl-C to exit.");

        // Run controllers only (no static server)
        run_controllers_only(controller_handles).await
    } else {
        // Standalone mode: create static server
        let bind_addr =
            env::var("RAUTA_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

        let num_cpus = num_cpus::get();
        info!(
            "🔥 Initializing per-core workers (lock-free mode): {} workers",
            num_cpus
        );
        let server = ProxyServer::new_with_workers(bind_addr.clone(), router, num_cpus)
            .map_err(|e| anyhow::anyhow!("Failed to create server: {}", e))?;

        info!(
            "🚀 HTTP proxy server listening on {} (per-core workers enabled)",
            bind_addr
        );
        info!("📋 Routes configured:");
        info!("   GET /api/*       -> 127.0.0.1:9090 (Python backend)");
        info!("");
        info!("Press Ctrl-C to exit.");

        // Run server with graceful shutdown (SIGTERM + Ctrl-C)
        run_with_signal_handling(server, controller_handles).await
    }
}

/// Run controllers only (K8s mode - listeners created dynamically)
/// Handles both SIGTERM (Kubernetes) and Ctrl-C (local development)
async fn run_controllers_only(controller_handles: Vec<tokio::task::JoinHandle<()>>) -> Result<()> {
    // Create shutdown signal that triggers on SIGTERM or Ctrl-C
    let shutdown_signal = async {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl-C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("Failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                info!("Received Ctrl-C signal");
            }
            _ = terminate => {
                info!("Received SIGTERM signal");
            }
        }
    };

    // Wait for shutdown signal
    shutdown_signal.await;

    info!("🛑 Shutting down controllers...");

    // Wait for all controller tasks to complete
    for handle in controller_handles {
        let _ = handle.await;
    }

    info!("✅ Controllers shut down gracefully");
    Ok(())
}

/// Run server with proper signal handling for graceful shutdown
/// Handles both SIGTERM (Kubernetes) and Ctrl-C (local development)
async fn run_with_signal_handling(
    server: ProxyServer,
    controller_handles: Vec<tokio::task::JoinHandle<()>>,
) -> Result<()> {
    // Create shutdown signal that triggers on SIGTERM or Ctrl-C
    let shutdown_signal = async {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl-C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("Failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                info!("Ctrl-C received, initiating graceful shutdown");
            },
            _ = terminate => {
                info!("SIGTERM received, initiating graceful shutdown");
            },
        }
    };

    // Run server with graceful shutdown support
    if let Err(e) = server.serve_with_shutdown(shutdown_signal).await {
        tracing::error!("Server error: {}", e);
    }

    // Cleanup: abort controller tasks
    info!("Shutting down Kubernetes controllers");
    for handle in controller_handles {
        handle.abort();
    }

    info!("Shutdown complete");
    Ok(())
}

/// Add example routes for testing
///
/// Week 4-5: Replace with K8s Ingress watcher
fn add_example_routes(router: &Arc<Router>) -> Result<()> {
    // Get backend from environment: RAUTA_BACKEND_ADDR (default: 127.0.0.1:9090)
    let backend_addr =
        env::var("RAUTA_BACKEND_ADDR").unwrap_or_else(|_| "127.0.0.1:9090".to_string());

    // Parse IP:port using rsplit_once to handle IPv6 addresses
    // IPv6 format: [::1]:8080, IPv4 format: 127.0.0.1:8080
    let (ip_str, port_str) = backend_addr.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid RAUTA_BACKEND_ADDR format. Expected IP:PORT, got: {}",
            backend_addr
        )
    })?;

    // Parse port first
    let port: u16 = port_str
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid backend port {}: {}", port_str, e))?;

    // Parse IPv4 address (strip brackets if present for IPv6 format)
    let ip_str = ip_str.trim_start_matches('[').trim_end_matches(']');
    let ip: Ipv4Addr = ip_str
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid backend IP {}: {}", ip_str, e))?;

    info!("   Backend: {}:{}", ip, port);

    // Route all paths to configured backend
    let backends = vec![Backend::new(u32::from(ip), port, 100)];

    router
        .add_route(HttpMethod::GET, "/", backends)
        .map_err(|e| anyhow::anyhow!("Failed to add route: {}", e))?;

    info!("📍 Backend configured: {}", backend_addr);

    Ok(())
}
