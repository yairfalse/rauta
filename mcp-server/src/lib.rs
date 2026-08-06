//! RAUTA MCP Server
//!
//! Provides MCP (Model Context Protocol) tools for AI agents to query and manage
//! the RAUTA gateway. Uses the `agent-api` crate's `GatewayQuery` trait for
//! transport-agnostic access to gateway state.
//!
//! ## MCP Tools
//!
//! | Tool | Description |
//! |---|---|
//! | `rauta_status` | Health overview: uptime, route count, open circuits |
//! | `rauta_list_routes` | All routes with backends |
//! | `rauta_get_route` | Single route detail |
//! | `rauta_metrics_snapshot` | Prometheus metrics as JSON |
//! | `rauta_list_circuit_breakers` | Circuit breaker states |
//! | `rauta_list_rate_limiters` | Rate limiter state |
//! | `rauta_diagnose` | Run diagnostics |
//! | `rauta_cache_stats` | Route cache stats |
//! | `rauta_list_listeners` | Active listeners |
//! | `rauta_timeline` | Recent events and compact snapshots |
//! | `rauta_diff` | Semantic recent-state diff |
//! | `rauta_tcp_health` | Optional TCP health evidence |
//! | `rauta_drain_backend` | Graceful drain with before/after evidence |
//! | `rauta_undrain_backend` | Cancel drain with before/after evidence |
//! | `rauta_quarantine_backend` | Bounded backend quarantine with expiry |
//!
//! ## Transports
//!
//! - **stdio**: For Claude Code / Cursor integration (`rauta --endpoint http://localhost:9091 mcp`)
//! - **Streamable HTTP**: `POST /mcp` on admin port 9091 (future)
//!
//! ## Usage
//!
//! The MCP server wraps a `GatewayQuery` implementation. In-process, this is
//! `LocalGatewayQuery`. For remote access, `RemoteGatewayQuery` (from rauta-cli).

pub mod handler;
