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
mod routes;

use apis::gateway::gateway::GatewayReconciler;
use apis::gateway::gateway_class::GatewayClassReconciler;
use apis::gateway::http_route::HTTPRouteReconciler;

/// RAUTA Control Plane - Stage 1
///
/// Pure Rust userspace HTTP proxy (no eBPF yet)
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
                let gw_reconciler = GatewayReconciler::new(gw_client, gw_name);
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

                info!("✅ Gateway API controllers started");
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

    // Determine bind address from environment variable or default
    let bind_addr = env::var("RAUTA_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    // Use per-core workers for lock-free performance (Stage 2!)
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
    if !k8s_mode {
        info!("📋 Routes configured:");
        info!("   GET /api/*       -> 127.0.0.1:9090 (Python backend)");
    }
    info!("");
    info!("Press Ctrl-C to exit.");

    // Run server with graceful shutdown
    tokio::select! {
        result = server.serve() => {
            if let Err(e) = result {
                tracing::error!("Server error: {}", e);
            }
        }
        _ = signal::ctrl_c() => {
            info!("Shutdown signal received");
        }
    }

    // Cleanup: abort controller tasks
    for handle in controller_handles {
        handle.abort();
    }

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

// ============================================================================
// Stage 2+: eBPF code (commented out for Stage 1)
// ============================================================================
// Will uncomment when we add eBPF observability in Stage 2 (Week 9-16)
/*
pub struct RautaControl {
    bpf: Ebpf,
}

impl RautaControl {
    /// Load XDP program and attach to interface
    pub async fn load(interface: &str, xdp_mode: &str) -> Result<Self, RautaError> {
        // Load BPF program from embedded bytes
        #[cfg(debug_assertions)]
        let mut bpf = Ebpf::load(include_bytes_aligned!(
            "../../target/bpfel-unknown-none/debug/rauta"
        ))
        .map_err(|e| RautaError::BpfLoadError(e.to_string()))?;

        #[cfg(not(debug_assertions))]
        let mut bpf = Ebpf::load(include_bytes_aligned!(
            "../../target/bpfel-unknown-none/release/rauta"
        ))
        .map_err(|e| RautaError::BpfLoadError(e.to_string()))?;

        // Get XDP program
        let program: &mut Xdp = bpf
            .program_mut("rauta_ingress")
            .ok_or_else(|| RautaError::BpfLoadError("rauta_ingress program not found".into()))?
            .try_into()
            .map_err(|e: aya::programs::ProgramError| {
                RautaError::BpfLoadError(format!("Failed to get XDP program: {}", e))
            })?;

        // Load the program
        program
            .load()
            .map_err(|e| RautaError::BpfLoadError(format!("Failed to load XDP program: {}", e)))?;

        // Parse XDP flags
        let flags = match xdp_mode {
            "native" | "driver" => XdpFlags::default(),
            "skb" | "generic" => XdpFlags::SKB_MODE,
            "hw" | "offload" => XdpFlags::HW_MODE,
            _ => {
                warn!(mode = xdp_mode, "Unknown XDP mode, defaulting to SKB_MODE");
                XdpFlags::SKB_MODE
            }
        };

        // Attach to interface
        program.attach(interface, flags).map_err(|e| {
            RautaError::BpfLoadError(format!(
                "Failed to attach XDP program to {}: {}",
                interface, e
            ))
        })?;

        info!(
            interface = interface,
            mode = xdp_mode,
            "XDP program attached successfully"
        );

        Ok(Self { bpf })
    }

    /// Add a test route for validation
    pub fn add_test_route(&mut self) -> Result<(), RautaError> {
        // Create route: GET /api/users
        let path = b"/api/users";
        let path_hash = fnv1a_hash(path);
        let route_key = RouteKey::new(HttpMethod::GET, path_hash);

        // Backend: 10.0.1.1:8080
        let backend = Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100, // weight
        );

        let mut backend_list = BackendList::empty();
        backend_list.backends[0] = backend;
        backend_list.count = 1;

        // Build compact Maglev table for this route (separate map!)
        let compact_table_vec = common::maglev_build_compact_table(&[backend]);
        let mut maglev_table = CompactMaglevTable::empty();
        maglev_table.table.copy_from_slice(&compact_table_vec);

        // Insert route into ROUTES map (small, fits in stack)
        {
            let mut routes: HashMap<_, RouteKey, BackendList> = HashMap::try_from(
                self.bpf
                    .map_mut("ROUTES")
                    .ok_or_else(|| RautaError::MapNotFound("ROUTES map not found".to_string()))?,
            )
            .map_err(|e| {
                RautaError::MapAccessError(format!("Failed to access ROUTES map: {}", e))
            })?;

            routes.insert(route_key, backend_list, 0).map_err(|e| {
                RautaError::MapAccessError(format!("Failed to insert route: {}", e))
            })?;
        }

        // Insert Maglev table into MAGLEV_TABLES map (indexed by path_hash)
        {
            let mut maglev_tables: HashMap<_, u64, CompactMaglevTable> =
                HashMap::try_from(self.bpf.map_mut("MAGLEV_TABLES").ok_or_else(|| {
                    RautaError::MapNotFound("MAGLEV_TABLES map not found".to_string())
                })?)
                .map_err(|e| {
                    RautaError::MapAccessError(format!("Failed to access MAGLEV_TABLES map: {}", e))
                })?;

            maglev_tables
                .insert(path_hash, maglev_table, 0)
                .map_err(|e| {
                    RautaError::MapAccessError(format!("Failed to insert Maglev table: {}", e))
                })?;
        }

        info!(
            method = "GET",
            path = "/api/users",
            backend = "10.0.1.1:8080",
            backends = backend_list.count,
            table_size = common::COMPACT_MAGLEV_SIZE,
            "Route added with separate compact Maglev table (avoids BPF stack overflow)"
        );

        Ok(())
    }

    /// Get metrics map for reporting
    pub fn metrics_map(&mut self) -> PerCpuArray<&mut aya::maps::MapData, Metrics> {
        PerCpuArray::try_from(self.bpf.map_mut("METRICS").expect("METRICS map not found"))
            .expect("Failed to access METRICS map")
    }

    /// Get routes map
    pub fn routes_map(&mut self) -> HashMap<&mut aya::maps::MapData, RouteKey, BackendList> {
        HashMap::try_from(self.bpf.map_mut("ROUTES").expect("ROUTES map not found"))
            .expect("Failed to access ROUTES map")
    }
}

/// Report metrics periodically
/// TODO: Re-enable when lifetime issues are resolved
#[allow(dead_code)]
async fn metrics_reporter(mut metrics: PerCpuArray<&mut aya::maps::MapData, Metrics>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

    loop {
        interval.tick().await;

        // Read per-CPU metrics and aggregate
        match metrics.get(&0, 0) {
            Ok(per_cpu_values) => {
                // Aggregate across all CPUs
                let mut total = Metrics::new();
                // Fix: PerCpuValues has iter() method
                for cpu_metrics in per_cpu_values.iter() {
                    total.packets_total += cpu_metrics.packets_total;
                    total.packets_tier1 += cpu_metrics.packets_tier1;
                    total.packets_tier2 += cpu_metrics.packets_tier2;
                    total.packets_tier3 += cpu_metrics.packets_tier3;
                    total.packets_dropped += cpu_metrics.packets_dropped;
                    total.http_parse_errors += cpu_metrics.http_parse_errors;
                }

                if total.packets_total > 0 {
                    info!(
                        packets_total = total.packets_total,
                        tier1 = total.packets_tier1,
                        tier2 = total.packets_tier2,
                        tier3 = total.packets_tier3,
                        dropped = total.packets_dropped,
                        parse_errors = total.http_parse_errors,
                        tier1_hit_rate = format!("{:.1}%", total.tier1_hit_rate() * 100.0),
                        "Metrics report"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to read metrics");
            }
        }
    }
}
*/
