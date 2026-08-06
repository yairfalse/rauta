//! RAUTA CLI — Gateway management tool and kubectl plugin
//!
//! Supports three output formats:
//! - `table` (default): Human-readable table output
//! - `json`: Machine-readable JSON
//! - `agent`: Compact self-describing JSON with `_meta` and `_hints` for LLM consumption

mod output;
mod remote_query;

use agent_api::query::GatewayQuery;
use agent_api::temporal::TemporalQuery;
use clap::{Parser, Subcommand, ValueEnum};
use std::process::Command as ProcessCommand;

#[derive(Parser)]
#[command(name = "rauta", version, about = "RAUTA gateway management CLI")]
struct Cli {
    /// Output format
    #[arg(long, default_value = "table", global = true)]
    format: OutputFormat,

    /// Admin server endpoint
    #[arg(
        long,
        default_value = "http://localhost:9091",
        global = true,
        env = "RAUTA_ADMIN_ENDPOINT"
    )]
    endpoint: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
    Agent,
}

#[derive(Subcommand)]
enum Commands {
    /// Show gateway status overview
    Status,

    /// Route management
    Routes {
        #[command(subcommand)]
        action: RouteAction,
    },

    /// Backend management
    Backends {
        #[command(subcommand)]
        action: BackendAction,
    },

    /// Metrics queries
    Metrics {
        #[command(subcommand)]
        action: MetricsAction,
    },

    /// Optional eBPF/kernel evidence
    Ebpf {
        #[command(subcommand)]
        action: EbpfAction,
    },

    /// Proof and demo workflows
    Proof {
        #[command(subcommand)]
        action: ProofAction,
    },

    /// Run diagnostics
    Diagnose {
        /// Symptom to diagnose (e.g., "high-latency", "circuit-breaker-cascade")
        symptom: String,

        /// Filter by route pattern
        #[arg(long)]
        route: Option<String>,

        /// Include temporal evidence from the last N seconds
        #[arg(long)]
        since_seconds: Option<u64>,
    },

    /// Show recent gateway temporal events and snapshots
    Timeline {
        /// Include history from the last N seconds
        #[arg(long)]
        since_seconds: Option<u64>,
    },

    /// Diff recent gateway state over a time window
    Diff {
        /// Diff against the earliest observation in the last N seconds
        #[arg(long)]
        since_seconds: Option<u64>,
    },

    /// Start MCP server over stdio (for Claude Code / Cursor integration)
    Mcp,
}

#[derive(Subcommand)]
enum RouteAction {
    /// List all routes
    List {
        /// Filter by HTTP method
        #[arg(long)]
        method: Option<String>,
    },
    /// Get route details
    Get {
        /// Route pattern
        pattern: String,
    },
}

#[derive(Subcommand)]
enum BackendAction {
    /// Show backend health
    Health {
        /// Filter by route
        #[arg(long)]
        route: Option<String>,
    },
    /// Drain a backend (graceful removal)
    Drain {
        /// Backend address (e.g., "10.0.1.5:8080")
        backend: String,
        /// Drain timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,
    },
    /// Cancel drain for a backend
    Undrain {
        /// Backend address
        backend: String,
    },
    /// Quarantine a backend until expiry
    Quarantine {
        /// Backend address
        backend: String,
        /// Quarantine TTL in seconds
        #[arg(long, default_value = "300")]
        ttl: u64,
    },
}

#[derive(Subcommand)]
enum MetricsAction {
    /// Snapshot of current metrics
    Snapshot,
    /// Query a specific metric
    Query {
        /// Metric name
        metric: String,
    },
}

#[derive(Subcommand)]
enum EbpfAction {
    /// Show optional TCP health evidence
    TcpHealth,
}

#[derive(Subcommand)]
enum ProofAction {
    /// Print or run the Kind incident demo workflow
    IncidentDemo {
        /// Execute the commands instead of printing the plan
        #[arg(long)]
        execute: bool,
        /// Kind cluster name
        #[arg(long, default_value = "rauta-proof")]
        cluster: String,
        /// Kubernetes namespace
        #[arg(long, default_value = "rauta-system")]
        namespace: String,
    },
}

#[derive(serde::Serialize)]
struct ProofStep {
    name: &'static str,
    command: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let client = remote_query::RemoteGatewayQuery::new(&cli.endpoint);

    match cli.command {
        Commands::Status => {
            let status = client.get_status().await?;
            output::render_status(&status, &cli.format);
        }
        Commands::Routes { action } => match action {
            RouteAction::List { method } => {
                let routes = client.get_routes(method.as_deref()).await?;
                output::render_routes(&routes, &cli.format);
            }
            RouteAction::Get { pattern } => {
                let route = client.get_route(&pattern).await?;
                output::render_route_detail(&route, &cli.format);
            }
        },
        Commands::Backends { action } => match action {
            BackendAction::Health { route } => {
                let routes = client.get_routes(None).await?;
                let backends = routes
                    .into_iter()
                    .filter(|route_snapshot| {
                        route
                            .as_deref()
                            .map(|filter| route_snapshot.pattern.contains(filter))
                            .unwrap_or(true)
                    })
                    .flat_map(|route_snapshot| route_snapshot.backends)
                    .collect::<Vec<_>>();
                output::render_backend_health(&backends, &cli.format);
            }
            BackendAction::Drain { backend, timeout } => {
                let result = client.drain_backend(&backend, timeout).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            BackendAction::Undrain { backend } => {
                let result = client.undrain_backend(&backend).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            BackendAction::Quarantine { backend, ttl } => {
                let result = client.quarantine_backend(&backend, ttl).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        },
        Commands::Metrics { action } => match action {
            MetricsAction::Snapshot => {
                let metrics = client.metrics_snapshot(None).await?;
                output::render_metrics(&metrics, &cli.format);
            }
            MetricsAction::Query { metric } => {
                let metrics = client.metrics_snapshot(Some(&metric)).await?;
                output::render_metrics(&metrics, &cli.format);
            }
        },
        Commands::Ebpf { action } => match action {
            EbpfAction::TcpHealth => {
                let evidence = client.tcp_health_evidence().await?;
                println!("{}", serde_json::to_string_pretty(&evidence)?);
            }
        },
        Commands::Proof { action } => match action {
            ProofAction::IncidentDemo {
                execute,
                cluster,
                namespace,
            } => {
                let steps = proof_incident_demo_steps(&cluster, &namespace);
                if execute {
                    run_proof_steps(&steps)?;
                } else {
                    println!("{}", serde_json::to_string_pretty(&steps)?);
                }
            }
        },
        Commands::Diagnose {
            symptom,
            route,
            since_seconds,
        } => {
            let diagnoses = client
                .diagnose_since(&symptom, route.as_deref(), None, since_seconds)
                .await?;
            output::render_diagnoses(&diagnoses, &cli.format);
        }
        Commands::Timeline { since_seconds } => {
            let timeline = client
                .timeline(TemporalQuery {
                    since_seconds,
                    ..TemporalQuery::default()
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&timeline)?);
        }
        Commands::Diff { since_seconds } => {
            let diff = client
                .diff(TemporalQuery {
                    since_seconds,
                    ..TemporalQuery::default()
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&diff)?);
        }
        Commands::Mcp => {
            // MCP server over stdio — stdout is the protocol channel, logs go to stderr
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::Level::INFO.into()),
                )
                .init();

            tracing::info!("Starting RAUTA MCP server (stdio transport)");
            tracing::info!("Admin endpoint: {}", cli.endpoint);

            let query: std::sync::Arc<dyn agent_api::query::GatewayQuery> =
                std::sync::Arc::new(remote_query::RemoteGatewayQuery::new(&cli.endpoint));
            let handler = mcp_server::handler::RautaMcpHandler::new(query);

            let service = rmcp::ServiceExt::serve(handler, rmcp::transport::stdio())
                .await
                .map_err(|e| anyhow::anyhow!("MCP serve error: {}", e))?;

            tracing::info!("MCP server running — waiting for client");
            service
                .waiting()
                .await
                .map_err(|e| anyhow::anyhow!("MCP wait error: {}", e))?;

            return Ok(());
        }
    }

    Ok(())
}

fn proof_incident_demo_steps(cluster: &str, namespace: &str) -> Vec<ProofStep> {
    vec![
        ProofStep {
            name: "create-kind-cluster",
            command: format!("kind create cluster --name {} --config deploy/kind-config.yaml", cluster),
        },
        ProofStep {
            name: "install-gateway-api",
            command: "kubectl apply -f deploy/gateway-api.yaml".to_string(),
        },
        ProofStep {
            name: "deploy-rauta",
            command: format!("kubectl create namespace {} --dry-run=client -o yaml | kubectl apply -f - && kubectl apply -f deploy/rauta-daemonset.yaml", namespace),
        },
        ProofStep {
            name: "deploy-demo-backend",
            command: "kubectl apply -f deploy/demo-backend.yaml".to_string(),
        },
        ProofStep {
            name: "send-traffic",
            command: "kubectl -n rauta-system port-forward svc/rauta-admin 9091:9091 & sleep 2 && rauta status --format=json".to_string(),
        },
        ProofStep {
            name: "inject-failure",
            command: "kubectl scale deploy/demo-backend --replicas=0".to_string(),
        },
        ProofStep {
            name: "diagnose",
            command: "rauta diagnose degraded --since-seconds=300 --format=agent".to_string(),
        },
        ProofStep {
            name: "safe-action",
            command: "rauta backends quarantine 127.0.0.1:8080 --ttl=300".to_string(),
        },
        ProofStep {
            name: "recover",
            command: "kubectl scale deploy/demo-backend --replicas=1 && rauta diff --since-seconds=300".to_string(),
        },
    ]
}

fn run_proof_steps(steps: &[ProofStep]) -> anyhow::Result<()> {
    for step in steps {
        eprintln!("==> {}", step.name);
        let status = ProcessCommand::new("sh")
            .arg("-lc")
            .arg(&step.command)
            .status()?;
        if !status.success() {
            anyhow::bail!("proof step '{}' failed with {}", step.name, status);
        }
    }
    Ok(())
}
