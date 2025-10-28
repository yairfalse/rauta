use anyhow::Result;
// Stage 1: Pure Rust userspace proxy (no eBPF)
// Stage 2+: Uncomment Aya for eBPF observability
// use aya::{...};
use tokio::signal;
use tracing::info;

mod error;
mod routes;
mod proxy;

/// RAUTA Control Plane - Stage 1
///
/// Pure Rust userspace HTTP proxy (no eBPF yet)
///
/// Responsibilities:
/// 1. HTTP proxy server (tokio + hyper) - Week 1-2
/// 2. K8s Ingress watcher (kube-rs) - Week 3-4
/// 3. Load balancing (Maglev) - Week 5-6
/// 4. Observability (metrics + UI) - Week 7-8
#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    info!("RAUTA Stage 1: Pure Rust Ingress Controller");
    info!("Starting HTTP proxy server...");

    // TODO Week 1-2: Start HTTP proxy server
    // TODO Week 3-4: Start K8s Ingress watcher
    // TODO Week 5-6: Add health checks
    // TODO Week 7-8: Start metrics server + UI

    // Wait for shutdown signal
    info!("Press Ctrl-C to exit.");
    signal::ctrl_c().await?;

    info!("Shutdown signal received");
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
