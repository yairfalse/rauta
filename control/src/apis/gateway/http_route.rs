//! HTTPRoute watcher
//!
//! Watches HTTPRoute resources and updates routing rules.

use crate::apis::metrics::record_httproute_reconciliation;
use crate::proxy::circuit_breaker::CircuitBreakerManager;
use crate::proxy::rate_limiter::RateLimiter;
use crate::proxy::router::Router;
use common::{Backend, HttpMethod};
use futures::StreamExt;
use gateway_api::apis::standard::httproutes::HTTPRoute;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{Client, ResourceExt};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// HTTPRoute reconciler
#[allow(dead_code)] // Used in K8s mode
pub struct HTTPRouteReconciler {
    client: Client,
    router: Arc<Router>,
    /// Gateway name to watch routes for
    gateway_name: String,
    rate_limiter: Arc<RateLimiter>,
    circuit_breaker: Arc<CircuitBreakerManager>,
}

/// HTTPRoute status parameters
struct RouteStatus {
    accepted: bool,
    resolved_refs: bool,
    resolved_refs_reason: String,
    resolved_refs_message: String,
    generation: i64,
}

#[allow(dead_code)] // Used in K8s mode
impl HTTPRouteReconciler {
    pub fn new(
        client: Client,
        router: Arc<Router>,
        gateway_name: String,
        rate_limiter: Arc<RateLimiter>,
        circuit_breaker: Arc<CircuitBreakerManager>,
    ) -> Self {
        Self {
            client,
            router,
            gateway_name,
            rate_limiter,
            circuit_breaker,
        }
    }

    /// Check if this HTTPRoute references our Gateway
    fn should_reconcile(
        &self,
        parent_refs: &Option<Vec<gateway_api::apis::standard::httproutes::HTTPRouteParentRefs>>,
    ) -> bool {
        if let Some(refs) = parent_refs {
            refs.iter().any(|r| r.name == self.gateway_name)
        } else {
            false
        }
    }

    /// Resolve Service name to Pod IPs via Kubernetes EndpointSlice API
    async fn resolve_service_endpoints(
        &self,
        service_name: &str,
        namespace: &str,
        service_port: u32,
    ) -> Result<Vec<Backend>, kube::Error> {
        use k8s_openapi::api::core::v1::Service;
        use k8s_openapi::api::discovery::v1::EndpointSlice;
        use kube::api::ListParams;

        // Fetch the Service to resolve service_port -> targetPort
        let service_api: Api<Service> = Api::namespaced(self.client.clone(), namespace);
        let service = service_api.get(service_name).await?;

        // Find the target port for the given service port
        let target_port = if let Some(spec) = &service.spec {
            if let Some(ports) = &spec.ports {
                ports
                    .iter()
                    .find(|p| p.port == service_port as i32)
                    .map(|p| {
                        // targetPort can be a number or a name
                        match &p.target_port {
                            Some(
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                                    port,
                                ),
                            ) => *port as u16,
                            Some(
                                k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(
                                    name,
                                ),
                            ) => {
                                // For named ports, we'd need to look at the Pod spec
                                // For now, fall back to service_port
                                warn!(
                                    "Named targetPort '{}' not yet supported, using service port",
                                    name
                                );
                                service_port as u16
                            }
                            None => service_port as u16, // Default to service port
                        }
                    })
                    .unwrap_or(service_port as u16)
            } else {
                service_port as u16
            }
        } else {
            service_port as u16
        };

        info!(
            "Service {}/{} port {} -> targetPort {}",
            namespace, service_name, service_port, target_port
        );

        let endpointslice_api: Api<EndpointSlice> = Api::namespaced(self.client.clone(), namespace);

        // List all EndpointSlices for this Service
        // EndpointSlices are labeled with kubernetes.io/service-name
        let list_params =
            ListParams::default().labels(&format!("kubernetes.io/service-name={}", service_name));

        match endpointslice_api.list(&list_params).await {
            Ok(endpointslice_list) => {
                let mut backends = Vec::new();

                // Merge backends from all EndpointSlices, matching the target port
                for endpointslice in endpointslice_list.items {
                    let slice_backends =
                        parse_endpointslice_to_backends(&endpointslice, target_port);
                    backends.extend(slice_backends);
                }

                if backends.is_empty() {
                    warn!(
                        "Service {}/{} has no ready endpoints",
                        namespace, service_name
                    );
                }

                info!(
                    "Resolved service {}/{} to {} backends",
                    namespace,
                    service_name,
                    backends.len()
                );

                Ok(backends)
            }
            Err(e) => {
                warn!(
                    "Failed to get endpointslices for service {}/{}: {}",
                    namespace, service_name, e
                );
                Err(e)
            }
        }
    }
}

/// Validate HTTP path according to Gateway API spec
///
/// Rules:
/// - Must start with "/"
/// - Must not be empty
/// - Must not have trailing slash (except root "/")
/// - Must not have double slashes
#[allow(dead_code)]
fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Path cannot be empty".to_string());
    }

    if !path.starts_with('/') {
        return Err(format!("Path '{}' must start with '/'", path));
    }

    if path.contains("//") {
        return Err(format!("Path '{}' cannot contain double slashes", path));
    }

    if path.len() > 1 && path.ends_with('/') {
        return Err(format!("Path '{}' cannot have trailing slash", path));
    }

    Ok(())
}

/// Validate hostname according to DNS-1123 subdomain spec
///
/// Rules:
/// - Lowercase alphanumeric characters, hyphens, and dots only
/// - Must not start or end with hyphen
/// - Must not have double dots
/// - Can start with wildcard "*."
/// - Max length 253 characters
#[allow(dead_code)]
fn validate_hostname(hostname: &str) -> Result<(), String> {
    if hostname.is_empty() {
        return Err("Hostname cannot be empty".to_string());
    }

    if hostname.len() > 253 {
        return Err(format!("Hostname '{}' exceeds 253 characters", hostname));
    }

    // Handle wildcard prefix
    let hostname_to_check = if let Some(stripped) = hostname.strip_prefix("*.") {
        stripped
    } else {
        hostname
    };

    if hostname_to_check.is_empty() {
        return Err("Hostname cannot be just '*.'".to_string());
    }

    // Check for double dots
    if hostname_to_check.contains("..") {
        return Err(format!("Hostname '{}' cannot contain '..'", hostname));
    }

    // Check each label
    for label in hostname_to_check.split('.') {
        if label.is_empty() {
            continue; // Skip empty labels (shouldn't happen after .. check)
        }

        // Must not start or end with hyphen
        if label.starts_with('-') {
            return Err(format!("Hostname label '{}' cannot start with '-'", label));
        }
        if label.ends_with('-') {
            return Err(format!("Hostname label '{}' cannot end with '-'", label));
        }

        // Must be lowercase alphanumeric or hyphen
        for c in label.chars() {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
                return Err(format!(
                    "Hostname '{}' contains invalid character '{}' (must be lowercase alphanumeric or hyphen)",
                    hostname, c
                ));
            }
        }
    }

    Ok(())
}

/// Validate HTTP header name according to RFC 7230
///
/// Rules:
/// - Must not be empty
/// - Must be 1-256 characters
/// - Cannot contain ":" (no HTTP/2 pseudo-headers)
/// - Cannot contain whitespace or control characters
/// - Must be ASCII printable (excluding special chars)
#[allow(dead_code)]
fn validate_header_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Header name cannot be empty".to_string());
    }

    if name.len() > 256 {
        return Err(format!("Header name '{}' exceeds 256 characters", name));
    }

    if name.contains(':') {
        return Err(format!(
            "Header name '{}' cannot contain ':' (HTTP/2 pseudo-headers not allowed)",
            name
        ));
    }

    for c in name.chars() {
        if c.is_whitespace() || c.is_control() {
            return Err(format!(
                "Header name '{}' contains invalid character (whitespace or control character)",
                name
            ));
        }
    }

    Ok(())
}

impl HTTPRouteReconciler {
    /// Reconcile a single HTTPRoute
    #[allow(dead_code)]
    async fn reconcile(route: Arc<HTTPRoute>, ctx: Arc<Self>) -> Result<Action, kube::Error> {
        let start = Instant::now();
        let namespace = route.namespace().unwrap_or_else(|| "default".to_string());
        let name = route.name_any();

        info!("Reconciling HTTPRoute: {}/{}", namespace, name);

        // Check if this HTTPRoute references our Gateway
        if !ctx.should_reconcile(&route.spec.parent_refs) {
            debug!(
                "HTTPRoute {}/{} does not reference our Gateway, ignoring",
                namespace, name
            );
            return Ok(Action::await_change());
        }

        info!(
            "HTTPRoute {}/{} references our Gateway, configuring routes",
            namespace, name
        );

        // Parse and configure routes
        let mut routes_added = 0;
        let mut all_backends_resolved = true;
        let mut resolution_error_reason = "ResolvedRefs".to_string();
        let mut resolution_error_message = "All backend refs resolved".to_string();

        if let Some(rules) = &route.spec.rules {
            for (rule_idx, rule) in rules.iter().enumerate() {
                // Extract path from matches (default to "/" if no matches)
                let path = if let Some(matches) = &rule.matches {
                    if let Some(first_match) = matches.first() {
                        if let Some(path_match) = &first_match.path {
                            path_match.value.as_deref().unwrap_or("/")
                        } else {
                            "/"
                        }
                    } else {
                        "/"
                    }
                } else {
                    "/"
                };

                // Validate path
                if let Err(validation_error) = validate_path(path) {
                    warn!(
                        "HTTPRoute {}/{} rule {} has invalid path '{}': {}",
                        namespace, name, rule_idx, path, validation_error
                    );
                    // Update status with validation error and return early
                    let generation = route.metadata.generation.unwrap_or(0);
                    ctx.set_route_status_invalid(
                        &namespace,
                        &name,
                        &format!("Invalid path: {}", validation_error),
                        generation,
                    )
                    .await?;

                    record_httproute_reconciliation(
                        &name,
                        &namespace,
                        start.elapsed().as_secs_f64(),
                        "validation_failed",
                    );

                    return Ok(Action::requeue(Duration::from_secs(300)));
                }

                // Extract backends
                if let Some(backend_refs) = &rule.backend_refs {
                    // First pass: resolve all services and collect weights
                    let mut resolved_services = Vec::new();
                    let mut total_weight = 0i32;

                    for backend_ref in backend_refs {
                        let service_name = &backend_ref.name;
                        let port = backend_ref.port.unwrap_or(80) as u32;
                        let weight = backend_ref.weight.unwrap_or(1);

                        info!(
                            "  - Resolving backend Service: {}/{}:{} (weight: {})",
                            namespace, service_name, port, weight
                        );

                        match ctx
                            .resolve_service_endpoints(service_name, &namespace, port)
                            .await
                        {
                            Ok(service_backends) => {
                                if service_backends.is_empty() {
                                    // Service exists but has no ready endpoints
                                    warn!(
                                        "Service {}/{} has no ready endpoints",
                                        namespace, service_name
                                    );
                                    all_backends_resolved = false;
                                    resolution_error_reason = "BackendNotFound".to_string();
                                    resolution_error_message = format!(
                                        "Service {}/{} has no ready endpoints",
                                        namespace, service_name
                                    );
                                } else {
                                    info!("    -> Resolved to {} Pod IPs", service_backends.len());
                                    resolved_services.push((service_backends, weight));
                                    total_weight += weight;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to resolve service {}/{}: {}",
                                    namespace, service_name, e
                                );
                                all_backends_resolved = false;
                                resolution_error_reason = "BackendNotFound".to_string();
                                resolution_error_message = format!(
                                    "Failed to resolve service {}/{}",
                                    namespace, service_name
                                );
                            }
                        }
                    }

                    // Second pass: build weighted backend list
                    // Strategy: Interleave backends proportionally to avoid truncation bias
                    const MAX_MAGLEV_BACKENDS: usize = 31;

                    let mut backends = Vec::new();

                    if total_weight > 0 {
                        // First, calculate target slots for each service using largest-remainder method
                        // This ensures slots sum to exactly MAX_MAGLEV_BACKENDS (more fair than simple rounding)
                        let mut service_targets = Vec::new();
                        let mut slot_allocations = Vec::new();

                        // Step 1: Calculate exact fractional slots and assign floor values
                        let mut allocated_slots = 0;
                        for (service_backends, weight) in &resolved_services {
                            let exact_slots =
                                (*weight as f64 / total_weight as f64) * MAX_MAGLEV_BACKENDS as f64;
                            let floor_slots = exact_slots.floor() as usize;
                            let remainder = exact_slots - floor_slots as f64;

                            slot_allocations.push((service_backends, floor_slots, remainder));
                            allocated_slots += floor_slots;
                        }

                        // Step 2: Distribute remaining slots to services with largest remainders
                        let remaining_slots = MAX_MAGLEV_BACKENDS - allocated_slots;
                        let mut indexed_remainders: Vec<(usize, f64)> = slot_allocations
                            .iter()
                            .enumerate()
                            .map(|(idx, (_, _, remainder))| (idx, *remainder))
                            .collect();

                        // Sort by remainder descending (largest first)
                        // Safe: remainders are always valid f64 values (not NaN)
                        #[allow(clippy::unwrap_used)]
                        indexed_remainders.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

                        // Give one extra slot to top N services (where N = remaining_slots)
                        for (idx, _remainder) in indexed_remainders
                            .iter()
                            .take(remaining_slots.min(indexed_remainders.len()))
                        {
                            slot_allocations[*idx].1 += 1;
                        }

                        // Step 3: Calculate replicas per pod for each service
                        for (service_backends, service_slots, _) in slot_allocations {
                            let pods_count = service_backends.len();
                            let replicas_per_pod =
                                (service_slots as f64 / pods_count as f64).ceil().max(1.0) as usize;

                            info!(
                                "  - Service gets {} slots, {} replicas per pod ({} pods)",
                                service_slots, replicas_per_pod, pods_count
                            );

                            service_targets.push((service_backends.clone(), replicas_per_pod));
                        }

                        // Interleave backends round-robin across services to avoid truncation bias
                        let max_replicas =
                            service_targets.iter().map(|(_, r)| *r).max().unwrap_or(0);
                        for replica_round in 0..max_replicas {
                            for (service_backends, replicas_per_pod) in &service_targets {
                                if replica_round < *replicas_per_pod {
                                    backends.extend(service_backends.clone());
                                }
                            }
                        }

                        // Trim to exactly MAX_MAGLEV_BACKENDS if we went over
                        if backends.len() > MAX_MAGLEV_BACKENDS {
                            backends.truncate(MAX_MAGLEV_BACKENDS);
                        }
                    }

                    info!(
                        "  - Final backend count: {} (max: {})",
                        backends.len(),
                        MAX_MAGLEV_BACKENDS
                    );

                    if !backends.is_empty() {
                        let backend_count = backends.len();
                        // Add route to router (using GET as default method)
                        match ctx
                            .router
                            .add_route(HttpMethod::GET, path, backends.clone())
                        {
                            Ok(_) => {
                                info!(
                                    "  - Added route: {} -> {} total backends",
                                    path, backend_count
                                );
                                routes_added += 1;

                                // Configure rate limiting from annotations
                                if let Some(annotations) = &route.metadata.annotations {
                                    if let Some(rate_limit_config) =
                                        annotations.get("rauta.io/rate-limit")
                                    {
                                        match parse_rate_limit_annotation(rate_limit_config) {
                                            Ok((rate, burst)) => {
                                                ctx.rate_limiter.configure_route(path, rate, burst);
                                                info!(
                                                    "  - Configured rate limit: {} ({}rps, burst={})",
                                                    path, rate, burst
                                                );
                                            }
                                            Err(e) => {
                                                warn!("Failed to parse rate limit annotation '{}': {}", rate_limit_config, e);
                                            }
                                        }
                                    }
                                }

                                // Note: Circuit breaker is per-backend, not per-route
                                // It's automatically configured with default thresholds for all backends
                                // Future: Support BackendPolicy for per-backend customization
                            }
                            Err(e) => {
                                warn!("Failed to add route {}: {}", path, e);
                            }
                        }
                    } else {
                        warn!(
                            "No backends found for route {} (all services failed to resolve)",
                            path
                        );
                    }
                } else {
                    debug!("Rule {} has no backend refs", rule_idx);
                }
            }
        }

        // Update HTTPRoute status
        let generation = route.metadata.generation.unwrap_or(0);
        ctx.set_route_status(
            &namespace,
            &name,
            RouteStatus {
                accepted: routes_added > 0,
                resolved_refs: all_backends_resolved,
                resolved_refs_reason: resolution_error_reason,
                resolved_refs_message: resolution_error_message,
                generation,
            },
        )
        .await?;

        // Record metrics
        record_httproute_reconciliation(
            &name,
            &namespace,
            start.elapsed().as_secs_f64(),
            "success",
        );

        // Requeue after 5 minutes for periodic reconciliation
        Ok(Action::requeue(Duration::from_secs(300)))
    }

    /// Update HTTPRoute status with Accepted condition
    #[allow(dead_code)]
    async fn set_route_status(
        &self,
        namespace: &str,
        name: &str,
        status: RouteStatus,
    ) -> Result<(), kube::Error> {
        let api: Api<HTTPRoute> = Api::namespaced(self.client.clone(), namespace);

        // Build conditions based on accepted and resolved_refs status
        let mut conditions = vec![json!({
            "type": "Accepted",
            "status": if status.accepted { "True" } else { "False" },
            "reason": if status.accepted { "Accepted" } else { "NoRules" },
            "message": if status.accepted {
                "HTTPRoute is accepted and configured"
            } else {
                "HTTPRoute has no valid routing rules"
            },
            "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "observedGeneration": status.generation,
        })];

        // Only add ResolvedRefs condition if route is accepted
        if status.accepted {
            conditions.push(json!({
                "type": "ResolvedRefs",
                "status": if status.resolved_refs { "True" } else { "False" },
                "reason": &status.resolved_refs_reason,
                "message": &status.resolved_refs_message,
                "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "observedGeneration": status.generation,
            }));
        }

        let status_json = json!({
            "status": {
                "parents": [{
                    "parentRef": {
                        "name": self.gateway_name,
                        "kind": "Gateway",
                    },
                    "controllerName": "rauta.io/gateway-controller",
                    "conditions": conditions
                }]
            }
        });

        api.patch_status(
            name,
            &PatchParams::apply("rauta-controller"),
            &Patch::Merge(&status_json),
        )
        .await?;

        info!(
            "Updated HTTPRoute {}/{} status: accepted={}",
            namespace, name, status.accepted
        );
        Ok(())
    }

    /// Update HTTPRoute status with validation error (Accepted: False)
    #[allow(dead_code)]
    async fn set_route_status_invalid(
        &self,
        namespace: &str,
        name: &str,
        error_message: &str,
        generation: i64,
    ) -> Result<(), kube::Error> {
        let api: Api<HTTPRoute> = Api::namespaced(self.client.clone(), namespace);
        let status = json!({
            "status": {
                "parents": [{
                    "parentRef": {
                        "name": self.gateway_name,
                        "kind": "Gateway",
                    },
                    "controllerName": "rauta.io/gateway-controller",
                    "conditions": [{
                        "type": "Accepted",
                        "status": "False",
                        "reason": "Invalid",
                        "message": error_message,
                        "lastTransitionTime": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        "observedGeneration": generation,
                    }]
                }]
            }
        });

        api.patch_status(
            name,
            &PatchParams::apply("rauta-controller"),
            &Patch::Merge(&status),
        )
        .await?;

        warn!(
            "Rejected HTTPRoute {}/{} with validation error: {}",
            namespace, name, error_message
        );
        Ok(())
    }

    /// Error handler for controller
    #[allow(dead_code)]
    fn error_policy(_obj: Arc<HTTPRoute>, error: &kube::Error, _ctx: Arc<Self>) -> Action {
        error!("HTTPRoute reconciliation error: {:?}", error);
        // Retry after 1 minute on errors
        Action::requeue(Duration::from_secs(60))
    }

    /// Start the HTTPRoute controller
    #[allow(dead_code)]
    pub async fn run(self) -> Result<(), kube::Error> {
        let api: Api<HTTPRoute> = Api::all(self.client.clone());
        let ctx = Arc::new(self);

        info!("Starting HTTPRoute controller");

        // NOTE: EndpointSlice watching
        //
        // Ideally, we would watch EndpointSlice resources and trigger HTTPRoute reconciliation
        // when pods scale up/down. However, kube-rs Controller::watches() requires a mapper
        // function that cannot perform async operations, making it impossible to query which
        // HTTPRoutes reference a given Service.
        //
        // Current behavior:
        // - HTTPRoutes are reconciled periodically (default: every 5 minutes)
        // - Reconciliation resolves Service -> EndpointSlice -> Pod IPs
        // - Backend pool is updated if Pod IPs changed
        // - Router's idempotent add_route() makes updates cheap
        //
        // For production, consider:
        // - Using a custom reflector/watcher for EndpointSlice
        // - Maintaining an in-memory index of Service -> HTTPRoute mappings
        // - Triggering reconciliation via a channel when EndpointSlice changes
        //
        // This is acceptable because:
        // - 5-minute lag for pod scaling is reasonable for most workloads
        // - Kubernetes typically takes 30-60s for pod readiness anyway
        // - Manual reconciliation can be triggered via `kubectl annotate`

        Controller::new(api, WatcherConfig::default())
            .run(Self::reconcile, Self::error_policy, ctx)
            .for_each(|res| async move {
                match res {
                    Ok(o) => debug!("Reconciled HTTPRoute: {:?}", o),
                    Err(e) => error!("Reconciliation error: {:?}", e),
                }
            })
            .await;

        Ok(())
    }
}

/// Parse EndpointSlice into Backend structs (supports both IPv4 and IPv6)
#[allow(dead_code)] // Used in K8s mode
fn parse_endpointslice_to_backends(
    endpoint_slice: &k8s_openapi::api::discovery::v1::EndpointSlice,
    target_port: u16,
) -> Vec<Backend> {
    let mut backends = Vec::new();

    // Match the target port in EndpointSlice ports
    let port = if let Some(ports) = &endpoint_slice.ports {
        ports
            .iter()
            .find(|p| p.port.map(|pnum| pnum as u16) == Some(target_port))
            .and_then(|p| p.port)
            .unwrap_or(target_port as i32) as u16
    } else {
        target_port
    };

    // Iterate through endpoints
    for endpoint in &endpoint_slice.endpoints {
        // Check if endpoint is ready (only use ready endpoints)
        let is_ready = if let Some(conditions) = &endpoint.conditions {
            conditions.ready.unwrap_or(false)
        } else {
            true // Default to ready if no conditions
        };

        if !is_ready {
            continue; // Skip non-ready endpoints
        }

        // Parse IP addresses (both IPv4 and IPv6)
        for address in &endpoint.addresses {
            // Try IPv4 first
            if let Ok(ipv4) = address.parse::<std::net::Ipv4Addr>() {
                backends.push(Backend::from_ipv4(ipv4, port, 100));
                continue;
            }

            // Try IPv6
            if let Ok(ipv6) = address.parse::<std::net::Ipv6Addr>() {
                backends.push(Backend::from_ipv6(ipv6, port, 100));
                continue;
            }

            // Neither IPv4 nor IPv6
            warn!("Failed to parse IP address {}: invalid format", address);
        }
    }

    backends
}

/// Parse rate limit annotation
///
/// Format: "100rps,burst=200" or "50rps" (burst defaults to rate)
///
/// Returns: (rate, burst) or error message
fn parse_rate_limit_annotation(annotation: &str) -> Result<(f64, u64), String> {
    let parts: Vec<&str> = annotation.split(',').collect();

    // Parse rate (required)
    let rate_str = parts
        .first()
        .ok_or_else(|| "Missing rate specification".to_string())?
        .trim();

    if !rate_str.ends_with("rps") {
        return Err(format!("Rate must end with 'rps', got: {}", rate_str));
    }

    let rate: f64 = rate_str
        .trim_end_matches("rps")
        .parse()
        .map_err(|_| format!("Invalid rate number: {}", rate_str))?;

    if rate <= 0.0 {
        return Err("Rate must be positive".to_string());
    }

    // Parse burst (optional, defaults to rate)
    let burst = if parts.len() > 1 {
        let burst_str = parts[1].trim();
        if !burst_str.starts_with("burst=") {
            return Err(format!("Expected 'burst=N', got: {}", burst_str));
        }

        burst_str
            .trim_start_matches("burst=")
            .parse()
            .map_err(|_| format!("Invalid burst number: {}", burst_str))?
    } else {
        rate.ceil() as u64 // Default burst to rate, rounding up fractional rates
    };

    Ok((rate, burst))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use gateway_api::apis::standard::httproutes::{HTTPRoute, HTTPRouteSpec};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    #[test]
    fn test_httproute_basic_structure() {
        // RED: This test documents basic HTTPRoute structure

        // Create a basic HTTPRoute
        let route = HTTPRoute {
            metadata: ObjectMeta {
                name: Some("api-route".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            spec: HTTPRouteSpec::default(),
            status: None,
        };

        // Verify basic structure
        assert_eq!(route.metadata.name, Some("api-route".to_string()));
        assert_eq!(route.metadata.namespace, Some("default".to_string()));

        // Note: Full reconcile() testing will verify the following:
        // 1. The reconciler parses HTTPRoute rules
        // 2. It adds routes to the Router with correct path matching
        // 3. It resolves Service endpoints from backendRefs
        // 4. It updates status with route acceptance
    }

    #[test]
    fn test_httproute_reconciler_creation() {
        // RED: This test documents HTTPRoute reconciler setup

        // Create a router for the reconciler
        let router = Arc::new(Router::new());

        // Router starts with no routes configured
        // (We can't easily test this without accessing private fields,
        // but the reconciler will add routes via add_route())

        // Note: Full reconciler.run() testing will verify the following:
        // 1. HTTPRoute resources are watched
        // 2. Routes are added to Router based on HTTPRoute spec
        // 3. Service endpoints are resolved from Kubernetes API
        // 4. Status is updated with route acceptance

        // For now, just verify router was created
        assert!(Arc::strong_count(&router) == 1);
    }

    #[test]
    fn test_httproute_metrics_recorded() {
        // RED: Test that HTTPRoute reconciliation records metrics
        // This test will FAIL until we implement metrics

        use crate::apis::metrics::gather_controller_metrics;

        // Record a fake reconciliation
        crate::apis::metrics::record_httproute_reconciliation(
            "test-route",
            "default",
            0.123,
            "success",
        );

        // Gather metrics and verify they contain the expected data
        let metrics = gather_controller_metrics().expect("Should gather metrics");

        assert!(
            metrics.contains("httproute_reconciliation_duration_seconds"),
            "Should contain duration metric"
        );
        assert!(
            metrics.contains("httproute_reconciliations_total"),
            "Should contain counter metric"
        );
    }

    #[test]
    fn test_endpointslice_to_backends() {
        // Test parsing EndpointSlice with ready endpoints
        use k8s_openapi::api::discovery::v1::{Endpoint, EndpointPort, EndpointSlice};

        // Create a mock EndpointSlice with 3 endpoints
        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.1".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.2".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.3".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
            ],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Parse into backends (target port 8080)
        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);

        // Verify we got 3 backends
        assert_eq!(backends.len(), 3, "Should have 3 backends");

        // Verify first backend
        assert_eq!(
            backends[0].as_ipv4().unwrap(),
            "10.0.1.1".parse::<std::net::Ipv4Addr>().unwrap()
        );
        assert_eq!(backends[0].port, 8080);
        assert_eq!(backends[0].weight, 100);

        // Verify second backend
        assert_eq!(
            backends[1].as_ipv4().unwrap(),
            "10.0.1.2".parse::<std::net::Ipv4Addr>().unwrap()
        );

        // Verify third backend
        assert_eq!(
            backends[2].as_ipv4().unwrap(),
            "10.0.1.3".parse::<std::net::Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn test_endpointslice_filters_not_ready() {
        // Test that non-ready endpoints are filtered out
        use k8s_openapi::api::discovery::v1::{
            Endpoint, EndpointConditions, EndpointPort, EndpointSlice,
        };

        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.1".to_string()],
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: Some(true),
                        terminating: Some(false),
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.2".to_string()],
                    conditions: Some(EndpointConditions {
                        ready: Some(false),
                        serving: Some(false),
                        terminating: Some(true),
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.3".to_string()],
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: Some(true),
                        terminating: Some(false),
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
            ],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);

        // Should only get 2 backends (the ready ones)
        assert_eq!(backends.len(), 2, "Should have 2 ready backends");

        // Verify we got the right IPs (not the not-ready one)
        let ips: Vec<String> = backends
            .iter()
            .map(|b| b.as_ipv4().unwrap().to_string())
            .collect();
        assert!(ips.contains(&"10.0.1.1".to_string()));
        assert!(!ips.contains(&"10.0.1.2".to_string())); // Not ready
        assert!(ips.contains(&"10.0.1.3".to_string()));
    }

    #[test]
    fn test_endpointslice_empty() {
        // Test EndpointSlice with no endpoints
        use k8s_openapi::api::discovery::v1::{EndpointPort, EndpointSlice};

        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);
        assert_eq!(backends.len(), 0, "Should have 0 backends");
    }

    #[test]
    fn test_endpointslice_default_port() {
        // Test EndpointSlice with no port specified
        use k8s_openapi::api::discovery::v1::{Endpoint, EndpointSlice};

        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(),
            endpoints: vec![Endpoint {
                addresses: vec!["10.0.1.1".to_string()],
                conditions: None,
                deprecated_topology: None,
                hints: None,
                hostname: None,
                node_name: None,
                target_ref: None,
                zone: None,
            }],
            metadata: Default::default(),
            ports: None, // No port specified
        };

        let backends = parse_endpointslice_to_backends(&endpoint_slice, 80);
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].port, 80, "Should default to port 80");
    }

    #[test]
    fn test_httproute_status_with_invalid_backend_ref() {
        // RED: Test that HTTPRoute sets ResolvedRefs=False when backend ref is invalid
        // This test documents what set_route_status() should do for invalid backends

        // Expected status structure when backend ref fails:
        // - Accepted=True (route syntax is valid)
        // - ResolvedRefs=False with reason "BackendNotFound" or "InvalidKind"
        // - observedGeneration should match metadata.generation

        // This test will PASS once we implement proper backend validation
        // For now, it documents the expected behavior
    }

    #[test]
    fn test_httproute_status_with_invalid_parent_ref() {
        // RED: Test that HTTPRoute sets Accepted=False when parent ref doesn't match
        // This test documents what set_route_status() should do for invalid parent refs

        // Expected status structure when parent ref is invalid:
        // - Accepted=False with reason "NoMatchingParent"
        // - sectionName mismatch should be detected
        // - Non-existent Gateway should be detected

        // This test will PASS once we implement proper parent ref validation
    }

    #[test]
    fn test_httproute_status_multiple_parent_refs() {
        // RED: Test that HTTPRoute can have status for multiple parent Gateways
        // Each parent should have its own status entry in status.parents[]

        // Expected behavior:
        // - HTTPRoute with multiple parentRefs gets status for each
        // - Each parent can have different Accepted/ResolvedRefs status
        // - controllerName should be set for each parent
    }

    #[test]
    fn test_endpointslice_ipv6_parsing() {
        // GREEN: Test that IPv6 addresses are parsed correctly in HTTPRoute
        use k8s_openapi::api::discovery::v1::{Endpoint, EndpointPort, EndpointSlice};

        // Create EndpointSlice with both IPv4 and IPv6 addresses
        let endpoint_slice = EndpointSlice {
            address_type: "IPv4".to_string(), // K8s uses this even for dual-stack
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.1".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["2001:db8::1".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["::1".to_string()],
                    conditions: None,
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
            ],
            metadata: Default::default(),
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Parse into backends (target port 8080)
        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);

        // Should have all 3 backends: 1 IPv4 + 2 IPv6
        assert_eq!(
            backends.len(),
            3,
            "Should have all backends (IPv4 and IPv6)"
        );

        // Find backends by address type
        let ipv4_backends: Vec<&Backend> = backends.iter().filter(|b| !b.is_ipv6()).collect();
        let ipv6_backends: Vec<&Backend> = backends.iter().filter(|b| b.is_ipv6()).collect();

        // Verify 1 IPv4 backend
        assert_eq!(ipv4_backends.len(), 1, "Should have 1 IPv4 backend");
        assert_eq!(
            ipv4_backends[0].as_ipv4().unwrap(),
            "10.0.1.1".parse::<std::net::Ipv4Addr>().unwrap()
        );

        // Verify 2 IPv6 backends
        assert_eq!(ipv6_backends.len(), 2, "Should have 2 IPv6 backends");
        let ipv6_addrs: Vec<std::net::Ipv6Addr> =
            ipv6_backends.iter().map(|b| b.as_ipv6().unwrap()).collect();
        assert!(ipv6_addrs.contains(&"2001:db8::1".parse().unwrap()));
        assert!(ipv6_addrs.contains(&"::1".parse().unwrap()));
    }

    #[test]
    fn test_validate_path_valid() {
        // RED: Test path validation accepts valid paths
        assert!(validate_path("/").is_ok(), "Root path should be valid");
        assert!(validate_path("/api").is_ok(), "Simple path should be valid");
        assert!(
            validate_path("/api/v1").is_ok(),
            "Nested path should be valid"
        );
        assert!(
            validate_path("/api/users/123").is_ok(),
            "Path with numbers should be valid"
        );
    }

    #[test]
    fn test_validate_path_invalid() {
        // RED: Test path validation rejects invalid paths
        assert!(validate_path("").is_err(), "Empty path should be rejected");
        assert!(
            validate_path("api").is_err(),
            "Path without leading slash should be rejected"
        );
        assert!(
            validate_path("//api").is_err(),
            "Path with double slash should be rejected"
        );
        assert!(
            validate_path("/api/").is_err(),
            "Path with trailing slash should be rejected"
        );
    }

    #[test]
    fn test_validate_hostname_valid() {
        // RED: Test hostname validation accepts valid DNS-1123 subdomains
        assert!(
            validate_hostname("example.com").is_ok(),
            "Simple hostname should be valid"
        );
        assert!(
            validate_hostname("api.example.com").is_ok(),
            "Subdomain should be valid"
        );
        assert!(
            validate_hostname("*.example.com").is_ok(),
            "Wildcard hostname should be valid"
        );
        assert!(
            validate_hostname("my-app-123.example.com").is_ok(),
            "Hostname with hyphens and numbers should be valid"
        );
    }

    #[test]
    fn test_validate_hostname_invalid() {
        // RED: Test hostname validation rejects invalid hostnames
        assert!(
            validate_hostname("").is_err(),
            "Empty hostname should be rejected"
        );
        assert!(
            validate_hostname("EXAMPLE.COM").is_err(),
            "Uppercase hostname should be rejected"
        );
        assert!(
            validate_hostname("example..com").is_err(),
            "Double dot should be rejected"
        );
        assert!(
            validate_hostname("-example.com").is_err(),
            "Leading hyphen should be rejected"
        );
        assert!(
            validate_hostname("example-.com").is_err(),
            "Trailing hyphen should be rejected"
        );
        assert!(
            validate_hostname("example_test.com").is_err(),
            "Underscore should be rejected"
        );
    }

    #[test]
    fn test_validate_header_name_valid() {
        // RED: Test header name validation accepts valid RFC 7230 header names
        assert!(
            validate_header_name("content-type").is_ok(),
            "Lowercase header should be valid"
        );
        assert!(
            validate_header_name("Content-Type").is_ok(),
            "Mixed case header should be valid"
        );
        assert!(
            validate_header_name("X-Custom-Header").is_ok(),
            "Custom header with hyphens should be valid"
        );
        assert!(
            validate_header_name("Authorization").is_ok(),
            "Single word header should be valid"
        );
    }

    #[test]
    fn test_validate_header_name_invalid() {
        // RED: Test header name validation rejects invalid header names
        assert!(
            validate_header_name("").is_err(),
            "Empty header name should be rejected"
        );
        assert!(
            validate_header_name(":authority").is_err(),
            "HTTP/2 pseudo-header should be rejected"
        );
        assert!(
            validate_header_name("content type").is_err(),
            "Header with space should be rejected"
        );
        assert!(
            validate_header_name("content:type").is_err(),
            "Header with colon should be rejected"
        );
        assert!(
            validate_header_name("content\ntype").is_err(),
            "Header with newline should be rejected"
        );
    }

    // TDD RED PHASE: HTTP Method Matching Tests
    //
    // These tests document the expected behavior for HTTP method matching
    // according to Gateway API v1 spec. Currently RAUTA hardcodes all routes
    // to HttpMethod::GET, which is incorrect.
    //
    // Gateway API spec requirement:
    // HTTPRoute.spec.rules[].matches[].method allows filtering by HTTP method.
    // When omitted, all methods match. When specified, only requests with that
    // method should be routed to the backends.

    #[tokio::test]
    async fn test_httproute_method_matching_post() {
        // RED: Test that POST requests route to POST-only backend
        //
        // Create HTTPRoute that routes POST /api/users to backend-post
        // and GET /api/users to backend-get. Verify router correctly
        // handles method-based routing.

        use crate::proxy::router::Router;
        use common::{Backend, HttpMethod};
        use std::net::Ipv4Addr;

        let router = Router::new();

        // Backend for POST requests
        let post_backend = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        // Backend for GET requests
        let get_backend = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 2)),
            8080,
            100,
        )];

        // Add routes with method-specific backends
        router
            .add_route(HttpMethod::POST, "/api/users", post_backend.clone())
            .expect("Should add POST route");

        router
            .add_route(HttpMethod::GET, "/api/users", get_backend.clone())
            .expect("Should add GET route");

        // Test POST request routes to POST backend
        let post_match = router
            .select_backend(HttpMethod::POST, "/api/users", None, None)
            .expect("Should find POST backend");

        assert_eq!(
            post_match.backend.as_ipv4().unwrap(),
            Ipv4Addr::new(10, 0, 1, 1),
            "POST request should route to POST backend (10.0.1.1)"
        );

        // Test GET request routes to GET backend
        let get_match = router
            .select_backend(HttpMethod::GET, "/api/users", None, None)
            .expect("Should find GET backend");

        assert_eq!(
            get_match.backend.as_ipv4().unwrap(),
            Ipv4Addr::new(10, 0, 1, 2),
            "GET request should route to GET backend (10.0.1.2)"
        );
    }

    #[tokio::test]
    async fn test_httproute_method_matching_all_methods() {
        // RED: Test support for all HTTP methods
        //
        // Gateway API spec supports: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS
        // Verify router can handle all of them

        use crate::proxy::router::Router;
        use common::{Backend, HttpMethod};
        use std::net::Ipv4Addr;

        let router = Router::new();

        // Create backends for each method (using different IPs)
        let methods_and_ips = vec![
            (HttpMethod::GET, Ipv4Addr::new(10, 0, 1, 1)),
            (HttpMethod::POST, Ipv4Addr::new(10, 0, 1, 2)),
            (HttpMethod::PUT, Ipv4Addr::new(10, 0, 1, 3)),
            (HttpMethod::DELETE, Ipv4Addr::new(10, 0, 1, 4)),
            (HttpMethod::PATCH, Ipv4Addr::new(10, 0, 1, 5)),
            (HttpMethod::HEAD, Ipv4Addr::new(10, 0, 1, 6)),
            (HttpMethod::OPTIONS, Ipv4Addr::new(10, 0, 1, 7)),
        ];

        // Add route for each method
        for (method, ip) in &methods_and_ips {
            let backend = vec![Backend::new(u32::from(*ip), 8080, 100)];
            router
                .add_route(*method, "/api/resource", backend)
                .unwrap_or_else(|_| panic!("Should add {:?} route", method));
        }

        // Verify each method routes to correct backend
        for (method, expected_ip) in &methods_and_ips {
            let route_match = router
                .select_backend(*method, "/api/resource", None, None)
                .unwrap_or_else(|| panic!("Should find {:?} backend", method));

            assert_eq!(
                route_match.backend.as_ipv4().unwrap(),
                *expected_ip,
                "{:?} request should route to correct backend",
                method
            );
        }
    }

    #[tokio::test]
    async fn test_httproute_method_mismatch_returns_404() {
        // RED: Test that method mismatch returns None (404)
        //
        // If HTTPRoute only configures POST /api/users, then
        // GET /api/users should return None (resulting in 404)

        use crate::proxy::router::Router;
        use common::{Backend, HttpMethod};
        use std::net::Ipv4Addr;

        let router = Router::new();

        // Only add POST route
        let backend = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];
        router
            .add_route(HttpMethod::POST, "/api/users", backend)
            .expect("Should add POST route");

        // GET request should return None (no matching route)
        let get_result = router.select_backend(HttpMethod::GET, "/api/users", None, None);

        assert!(
            get_result.is_none(),
            "GET request should fail (no GET route configured)"
        );

        // POST request should succeed
        let post_result = router.select_backend(HttpMethod::POST, "/api/users", None, None);

        assert!(post_result.is_some(), "POST request should succeed");
    }

    #[tokio::test]
    async fn test_httproute_header_matching_exact() {
        // RED: Test that requests with specific headers route correctly
        use crate::proxy::router::{HeaderMatch, HeaderMatchType, Router};
        use common::{Backend, HttpMethod};
        use std::net::Ipv4Addr;

        let router = Router::new();

        // Backend for route with header constraint
        let v2_backend = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 2, 1)),
            8080,
            100,
        )];

        // Add route with X-API-Version: v2 header constraint
        let v2_headers = vec![HeaderMatch {
            name: "X-API-Version".to_string(),
            value: "v2".to_string(),
            match_type: HeaderMatchType::Exact,
        }];
        router
            .add_route_with_headers(
                HttpMethod::GET,
                "/api/v2/users",
                v2_backend.clone(),
                v2_headers,
            )
            .expect("Should add v2 route");

        // Test request with matching header
        let v2_req_headers = vec![("X-API-Version", "v2")];
        let v2_match = router
            .select_backend_with_headers(
                HttpMethod::GET,
                "/api/v2/users",
                v2_req_headers,
                None,
                None,
            )
            .expect("Should find v2 backend");

        assert_eq!(
            v2_match.backend.as_ipv4().unwrap(),
            Ipv4Addr::new(10, 0, 2, 1),
            "Request with X-API-Version: v2 should route to v2 backend"
        );

        // Test request with non-matching header (should fail)
        let wrong_headers = vec![("X-API-Version", "v1")];
        let no_match = router.select_backend_with_headers(
            HttpMethod::GET,
            "/api/v2/users",
            wrong_headers,
            None,
            None,
        );

        assert!(
            no_match.is_none(),
            "Request with wrong header value should not match"
        );

        // Test request with no headers (should fail)
        let empty_headers = vec![];
        let no_headers_match = router.select_backend_with_headers(
            HttpMethod::GET,
            "/api/v2/users",
            empty_headers,
            None,
            None,
        );

        assert!(
            no_headers_match.is_none(),
            "Request with no headers should not match route with header constraints"
        );
    }

    #[tokio::test]
    async fn test_httproute_header_matching_multiple_headers() {
        // RED: Test matching multiple headers
        use crate::proxy::router::{HeaderMatch, HeaderMatchType, Router};
        use common::{Backend, HttpMethod};
        use std::net::Ipv4Addr;

        let router = Router::new();

        let backend = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 3, 1)),
            8080,
            100,
        )];

        // Add route requiring both X-API-Version: v2 AND X-Client-Type: mobile
        let required_headers = vec![
            HeaderMatch {
                name: "X-API-Version".to_string(),
                value: "v2".to_string(),
                match_type: HeaderMatchType::Exact,
            },
            HeaderMatch {
                name: "X-Client-Type".to_string(),
                value: "mobile".to_string(),
                match_type: HeaderMatchType::Exact,
            },
        ];

        router
            .add_route_with_headers(
                HttpMethod::GET,
                "/api/mobile",
                backend.clone(),
                required_headers,
            )
            .expect("Should add route");

        // Request with both headers should match
        let matching_headers = vec![("X-API-Version", "v2"), ("X-Client-Type", "mobile")];

        let match_result = router.select_backend_with_headers(
            HttpMethod::GET,
            "/api/mobile",
            matching_headers,
            None,
            None,
        );

        assert!(
            match_result.is_some(),
            "Request with all required headers should match"
        );

        // Request with only one header should NOT match
        let partial_headers = vec![("X-API-Version", "v2")];

        let no_match = router.select_backend_with_headers(
            HttpMethod::GET,
            "/api/mobile",
            partial_headers,
            None,
            None,
        );

        assert!(
            no_match.is_none(),
            "Request missing required headers should not match"
        );
    }

    #[tokio::test]
    async fn test_httproute_header_case_insensitive() {
        // RED: Test that header names are case-insensitive (per HTTP spec)
        use crate::proxy::router::{HeaderMatch, HeaderMatchType, Router};
        use common::{Backend, HttpMethod};
        use std::net::Ipv4Addr;

        let router = Router::new();

        let backend = vec![Backend::new(
            u32::from(Ipv4Addr::new(10, 0, 4, 1)),
            8080,
            100,
        )];

        // Add route with lowercase header name
        let route_headers = vec![HeaderMatch {
            name: "x-api-version".to_string(),
            value: "v3".to_string(),
            match_type: HeaderMatchType::Exact,
        }];

        router
            .add_route_with_headers(HttpMethod::GET, "/api/test", backend.clone(), route_headers)
            .expect("Should add route");

        // Request with uppercase header name should match
        let req_headers = vec![("X-API-VERSION", "v3")];

        let match_result = router.select_backend_with_headers(
            HttpMethod::GET,
            "/api/test",
            req_headers,
            None,
            None,
        );

        assert!(
            match_result.is_some(),
            "Header names should be case-insensitive"
        );

        // Mixed case should also match
        let mixed_headers = vec![("X-Api-Version", "v3")];

        let mixed_match = router.select_backend_with_headers(
            HttpMethod::GET,
            "/api/test",
            mixed_headers,
            None,
            None,
        );

        assert!(
            mixed_match.is_some(),
            "Header names with mixed case should match"
        );
    }
}

// Note: HTTPRoute watcher implementation is planned for Phase 1
