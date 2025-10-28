use anyhow::Result;
use common::{Backend, HttpMethod};
use proxy::router::Router;
use proxy::server::ProxyServer;
use std::net::Ipv4Addr;
use tokio::signal;
use tracing::info;

mod error;
mod proxy;
mod routes;

/// RAUTA Control Plane - Stage 1
///
/// Pure Rust userspace HTTP proxy (no eBPF yet)
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("🦀 RAUTA Stage 1: Pure Rust Ingress Controller");

    // Create router and add example routes
    let router = Router::new();
    add_example_routes(&router)?;

    // Start HTTP proxy server
    let server = ProxyServer::new("127.0.0.1:8080".to_string(), router)
        .map_err(|e| anyhow::anyhow!("Failed to create server: {}", e))?;

    info!("🚀 HTTP proxy server listening on 127.0.0.1:8080");
    info!("📋 Routes configured:");
    info!("   GET /api/users   -> user-service (2 backends)");
    info!("   GET /api/posts   -> post-service (1 backend)");
    info!("   GET /health      -> health-service");
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

    Ok(())
}

/// Add example routes for testing
///
/// Week 4-5: Replace with K8s Ingress watcher
fn add_example_routes(router: &Router) -> Result<()> {
    // Route 1: GET /api/users -> user-service (2 backends for load balancing)
    let user_backends = vec![
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 1)), 8080, 100),
        Backend::new(u32::from(Ipv4Addr::new(10, 0, 1, 2)), 8080, 100),
    ];
    router
        .add_route(HttpMethod::GET, "/api/users", user_backends)
        .map_err(|e| anyhow::anyhow!("Failed to add route: {}", e))?;

    // Route 2: GET /api/posts -> post-service (single backend)
    let post_backends = vec![Backend::new(
        u32::from(Ipv4Addr::new(10, 0, 2, 1)),
        8080,
        100,
    )];
    router
        .add_route(HttpMethod::GET, "/api/posts", post_backends)
        .map_err(|e| anyhow::anyhow!("Failed to add route: {}", e))?;

    // Route 3: GET /health -> health endpoint
    let health_backends = vec![Backend::new(
        u32::from(Ipv4Addr::new(127, 0, 0, 1)),
        8080,
        100,
    )];
    router
        .add_route(HttpMethod::GET, "/health", health_backends)
        .map_err(|e| anyhow::anyhow!("Failed to add route: {}", e))?;

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
