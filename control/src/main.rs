use anyhow::{Context, Result};
use aya::{
    include_bytes_aligned,
    maps::{HashMap, LruHashMap, PerCpuArray},
    programs::{Xdp, XdpFlags},
    Bpf,
};
use common::{BackendList, Metrics, RouteKey};
use std::net::Ipv4Addr;
use tokio::signal;
use tracing::{info, warn};

mod error;
mod routes;

use error::RautaError;

/// RAUTA Control Plane
///
/// Responsibilities:
/// 1. Load XDP program onto network interface
/// 2. Manage BPF maps (routes, flow cache, metrics)
/// 3. Watch Kubernetes Ingress resources
/// 4. Provide API for route configuration
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    info!("RAUTA Control Plane starting...");

    // Parse command-line arguments
    let interface = std::env::var("RAUTA_INTERFACE").unwrap_or_else(|_| "eth0".to_string());
    let xdp_mode = std::env::var("RAUTA_XDP_MODE").unwrap_or_else(|_| "skb".to_string());

    info!(
        interface = %interface,
        xdp_mode = %xdp_mode,
        "Configuration loaded"
    );

    // Load eBPF program
    let mut control = RautaControl::load(&interface, &xdp_mode)
        .await
        .context("Failed to load RAUTA control plane")?;

    info!("XDP program loaded successfully");

    // Add example route for testing
    control
        .add_test_route()
        .context("Failed to add test route")?;

    info!("Test route added: GET /api/users -> 10.0.1.1:8080");

    // Start metrics reporting
    let metrics_handle = tokio::spawn({
        let metrics_map = control.metrics_map();
        async move {
            metrics_reporter(metrics_map).await;
        }
    });

    // Wait for shutdown signal
    info!("Control plane running. Press Ctrl-C to exit.");
    signal::ctrl_c().await?;

    info!("Shutdown signal received, cleaning up...");

    // Wait for metrics reporter to finish
    metrics_handle.abort();

    info!("RAUTA Control Plane stopped");
    Ok(())
}

/// Main control structure
pub struct RautaControl {
    bpf: Bpf,
}

impl RautaControl {
    /// Load XDP program and attach to interface
    pub async fn load(interface: &str, xdp_mode: &str) -> Result<Self, RautaError> {
        // Load BPF program from embedded bytes
        #[cfg(debug_assertions)]
        let mut bpf = Bpf::load(include_bytes_aligned!(
            "../../target/bpfel-unknown-none/debug/rauta"
        ))
        .map_err(|e| RautaError::BpfLoadError(e.to_string()))?;

        #[cfg(not(debug_assertions))]
        let mut bpf = Bpf::load(include_bytes_aligned!(
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
        program.load().map_err(|e| {
            RautaError::BpfLoadError(format!("Failed to load XDP program: {}", e))
        })?;

        // Parse XDP flags
        let flags = match xdp_mode {
            "native" | "driver" => XdpFlags::default(),
            "skb" | "generic" => XdpFlags::SKB_MODE,
            "hw" | "offload" => XdpFlags::HW_MODE,
            _ => {
                warn!(
                    mode = xdp_mode,
                    "Unknown XDP mode, defaulting to SKB_MODE"
                );
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
        use common::{fnv1a_hash, Backend, HttpMethod};

        let mut routes: HashMap<_, RouteKey, BackendList> =
            HashMap::try_from(self.bpf.map_mut("ROUTES").ok_or_else(|| {
                RautaError::MapNotFound("ROUTES map not found".to_string())
            })?)
            .map_err(|e| RautaError::MapAccessError(format!("Failed to access ROUTES map: {}", e)))?;

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

        // Insert route into BPF map
        routes
            .insert(route_key, backend_list, 0)
            .map_err(|e| RautaError::MapAccessError(format!("Failed to insert route: {}", e)))?;

        info!(
            method = "GET",
            path = "/api/users",
            backend = "10.0.1.1:8080",
            "Route added successfully"
        );

        Ok(())
    }

    /// Get metrics map for reporting
    pub fn metrics_map(&mut self) -> PerCpuArray<&aya::maps::MapData, Metrics> {
        PerCpuArray::try_from(self.bpf.map_mut("METRICS").expect("METRICS map not found"))
            .expect("Failed to access METRICS map")
    }

    /// Get routes map
    pub fn routes_map(&mut self) -> HashMap<&aya::maps::MapData, RouteKey, BackendList> {
        HashMap::try_from(self.bpf.map_mut("ROUTES").expect("ROUTES map not found"))
            .expect("Failed to access ROUTES map")
    }

    /// Get flow cache map
    pub fn flow_cache_map(&mut self) -> LruHashMap<&aya::maps::MapData, u64, u32> {
        LruHashMap::try_from(self.bpf.map_mut("FLOW_CACHE").expect("FLOW_CACHE map not found"))
            .expect("Failed to access FLOW_CACHE map")
    }
}

/// Report metrics periodically
async fn metrics_reporter(mut metrics: PerCpuArray<&aya::maps::MapData, Metrics>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

    loop {
        interval.tick().await;

        // Read per-CPU metrics and aggregate
        match metrics.get(&0, 0) {
            Ok(per_cpu_values) => {
                // Aggregate across all CPUs
                let mut total = Metrics::new();
                for cpu_metrics in per_cpu_values {
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
