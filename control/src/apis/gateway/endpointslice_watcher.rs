//! EndpointSlice watcher for dynamic backend updates
//!
//! Watches EndpointSlice resources and automatically updates Router backends
//! when Pods scale up/down (zero-downtime backend discovery).
//!
//! ## Usage
//!
//! ```ignore
//! use control::apis::gateway::endpointslice_watcher::watch_endpointslices;
//! use control::proxy::router::Router;
//! use kube::Client;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = Client::try_default().await?;
//!     let router = Arc::new(Router::new());
//!
//!     // Start watching EndpointSlices
//!     watch_endpointslices(client, router).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ## How It Works
//!
//! 1. Watch all EndpointSlice resources across namespaces
//! 2. Filter EndpointSlices by label: `kubernetes.io/service-name`
//! 3. When EndpointSlice changes (Pod added/removed):
//!    - Parse new backend list
//!    - Update Router with new backends
//!    - Rebuild Maglev table (minimal disruption)
//! 4. Zero-downtime: existing connections continue, new requests use new backends

use crate::proxy::router::Router;
use common::{Backend, HttpMethod};
use futures::StreamExt;
use k8s_openapi::api::discovery::v1::EndpointSlice;
use kube::runtime::watcher;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::{api::Api, Client, ResourceExt};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Maps Service name to EndpointSlice data
/// Key: "namespace/service-name"
type ServiceKey = String;

/// Watch EndpointSlices and update Router on changes
#[allow(dead_code)] // Used in K8s mode
pub async fn watch_endpointslices(
    client: Client,
    router: Arc<Router>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Watch all EndpointSlices across all namespaces
    let api: Api<EndpointSlice> = Api::all(client);
    let watcher = watcher(api, WatcherConfig::default());

    futures::pin_mut!(watcher);

    info!("Starting EndpointSlice watcher");

    // Track which services we're watching (service-name -> route path)
    let mut service_routes: HashMap<ServiceKey, Vec<String>> = HashMap::new();

    // Track ALL EndpointSlices per Service (per K8s docs: "there will be at least two")
    // Key: "namespace/service-name", Value: HashMap of slice-name -> EndpointSlice
    let mut service_slices: HashMap<ServiceKey, HashMap<String, EndpointSlice>> = HashMap::new();

    while let Some(event) = watcher.next().await {
        match event {
            Ok(watcher::Event::Apply(endpointslice))
            | Ok(watcher::Event::InitApply(endpointslice)) => {
                if let Err(e) = handle_endpointslice_apply(
                    &endpointslice,
                    &router,
                    &mut service_routes,
                    &mut service_slices,
                )
                .await
                {
                    warn!(
                        "Failed to handle EndpointSlice {}/{}: {}",
                        endpointslice
                            .namespace()
                            .unwrap_or_else(|| "default".to_string()),
                        endpointslice.name_any(),
                        e
                    );
                }
            }
            Ok(watcher::Event::Delete(endpointslice)) => {
                let namespace = endpointslice
                    .namespace()
                    .unwrap_or_else(|| "default".to_string());
                let name = endpointslice.name_any();
                debug!("EndpointSlice deleted: {}/{}", namespace, name);

                // Remove the deleted EndpointSlice from service_slices
                let service_name = endpointslice
                    .labels()
                    .get("kubernetes.io/service-name")
                    .cloned()
                    .unwrap_or_default();
                let service_key = format!("{}/{}", namespace, service_name);
                if let Some(slices) = service_slices.get_mut(&service_key) {
                    slices.remove(&name);
                    // If no EndpointSlices remain for this service, clean up tracking
                    if slices.is_empty() {
                        debug!(
                            "All EndpointSlices for service {} deleted; cleaning up tracking",
                            service_key
                        );
                        // Note: We don't remove routes from router here - routes persist until explicitly updated
                        service_slices.remove(&service_key);
                        service_routes.remove(&service_key);
                    }
                }
            }
            Ok(watcher::Event::Init) => {
                debug!("EndpointSlice watcher initialized");
            }
            Ok(watcher::Event::InitDone) => {
                info!("EndpointSlice watcher initial sync complete");
            }
            Err(e) => {
                warn!("EndpointSlice watcher error: {}", e);
            }
        }
    }

    Ok(())
}

/// Handle EndpointSlice apply/update event
///
/// Per K8s docs: "there will be at least two EndpointSlice objects" for a Service.
/// We must aggregate ALL slices for a Service, not just process one at a time.
async fn handle_endpointslice_apply(
    endpointslice: &EndpointSlice,
    router: &Arc<Router>,
    service_routes: &mut HashMap<ServiceKey, Vec<String>>,
    service_slices: &mut HashMap<ServiceKey, HashMap<String, EndpointSlice>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let namespace = endpointslice
        .namespace()
        .unwrap_or_else(|| "default".to_string());
    let name = endpointslice.name_any();

    // Extract service name from label
    let service_name = endpointslice
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("kubernetes.io/service-name"))
        .ok_or_else(|| {
            format!(
                "EndpointSlice {}/{} missing kubernetes.io/service-name label",
                namespace, name
            )
        })?;

    let service_key = format!("{}/{}", namespace, service_name);

    debug!(
        "EndpointSlice {}/{} updated for service {}",
        namespace, name, service_key
    );

    // Store this EndpointSlice in the service's slice map
    service_slices
        .entry(service_key.clone())
        .or_default()
        .insert(name.clone(), endpointslice.clone());

    // Determine the target port from the EndpointSlice
    let target_port = endpointslice
        .ports
        .as_ref()
        .and_then(|ports| {
            // Prefer port named "http", else take the first port
            ports
                .iter()
                .find(|p| p.name.as_deref() == Some("http"))
                .or_else(|| ports.first())
                .and_then(|p| p.port)
        })
        .unwrap_or_else(|| {
            warn!(
                "Could not determine target port from EndpointSlice {}/{}; defaulting to 8080",
                namespace, name
            );
            8080
        });

    // Aggregate backends from ALL EndpointSlices for this Service
    let all_backends =
        aggregate_backends_for_service(service_slices, &service_key, target_port as u16);

    if all_backends.is_empty() {
        warn!(
            "Service {} has no ready endpoints after aggregating all slices",
            service_key
        );
        return Ok(());
    }

    info!(
        "Service {} has {} ready backends (aggregated from {} slices)",
        service_key,
        all_backends.len(),
        service_slices
            .get(&service_key)
            .map(|s| s.len())
            .unwrap_or(0)
    );

    // Update Router for all routes that use this service
    // Note: Gateway API HTTPRoute doesn't specify method, so we use GET (matches all methods in router)
    if let Some(routes) = service_routes.get(&service_key) {
        for route_path in routes {
            router.update_route_backends(HttpMethod::GET, route_path, all_backends.clone())?;
            info!(
                "Updated route '{}' with {} backends from service {}",
                route_path,
                all_backends.len(),
                service_key
            );
        }
    }

    Ok(())
}

/// Aggregate backends from ALL EndpointSlices for a Service (with deduplication)
///
/// Per K8s docs: "clients must iterate through all EndpointSlices and deduplicate endpoints"
fn aggregate_backends_for_service(
    service_slices: &HashMap<ServiceKey, HashMap<String, EndpointSlice>>,
    service_key: &str,
    target_port: u16,
) -> Vec<Backend> {
    let mut backends = Vec::new();
    let mut seen_ips = std::collections::HashSet::new();

    // Get all slices for this service
    if let Some(slices) = service_slices.get(service_key) {
        for (_slice_name, endpointslice) in slices.iter() {
            // Parse backends from this slice
            let slice_backends = parse_endpointslice_to_backends(endpointslice, target_port);

            // Deduplicate by IP (a backend might appear in multiple slices)
            for backend in slice_backends {
                if seen_ips.insert(backend.ipv4) {
                    backends.push(backend);
                }
            }
        }
    }

    backends
}

/// Parse EndpointSlice to Backend list
///
/// **IPv4-only limitation**: Due to eBPF POD-compatible constraints in the `common::Backend`
/// struct (which stores IP as `u32`), only IPv4 addresses are supported. IPv6 addresses
/// are logged as warnings and gracefully skipped without panicking.
///
/// (Reuse from http_route.rs)
fn parse_endpointslice_to_backends(
    endpoint_slice: &EndpointSlice,
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

        // Parse IP addresses (IPv4 only - IPv6 not supported due to Backend struct constraint)
        for address in &endpoint.addresses {
            match address.parse::<std::net::Ipv4Addr>() {
                Ok(ipv4) => {
                    backends.push(Backend {
                        ipv4: u32::from(ipv4),
                        port,
                        weight: 100, // Default weight
                    });
                }
                Err(_) => {
                    // IPv6 addresses contain colons, IPv4 addresses do not
                    if address.contains(':') {
                        warn!(
                            "IPv6 address {} not supported (Backend struct is IPv4-only for eBPF compatibility), skipping",
                            address
                        );
                    } else {
                        warn!("Failed to parse IP address {}: invalid format", address);
                    }
                }
            }
        }
    }

    backends
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::router::Router;
    use common::HttpMethod;
    use k8s_openapi::api::discovery::v1::{Endpoint, EndpointConditions, EndpointPort};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    /// RED: Test that EndpointSlice updates trigger Router backend updates
    #[tokio::test]
    async fn test_endpointslice_updates_router_backends() {
        // Create Router with initial route
        let router = Arc::new(Router::new());

        let initial_backends = vec![Backend::new(
            u32::from(std::net::Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        router
            .add_route(HttpMethod::GET, "/api/users", initial_backends.clone())
            .expect("Should add initial route");

        // Verify initial backend
        let route_match = router
            .select_backend(HttpMethod::GET, "/api/users", None, None)
            .expect("Should find initial backend");

        assert_eq!(
            std::net::Ipv4Addr::from(route_match.backend.ipv4),
            std::net::Ipv4Addr::new(10, 0, 1, 1),
            "Initial backend should be 10.0.1.1"
        );

        // Create EndpointSlice with NEW backends (simulating Pod scale-up)
        let mut labels = BTreeMap::new();
        labels.insert(
            "kubernetes.io/service-name".to_string(),
            "api-service".to_string(),
        );

        let endpointslice = EndpointSlice {
            metadata: ObjectMeta {
                name: Some("api-service-abc123".to_string()),
                namespace: Some("default".to_string()),
                labels: Some(labels),
                ..Default::default()
            },
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.2".to_string()],
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
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Track service -> route mapping
        let mut service_routes: HashMap<ServiceKey, Vec<String>> = HashMap::new();
        service_routes.insert(
            "default/api-service".to_string(),
            vec!["/api/users".to_string()],
        );

        // Track EndpointSlices per service
        let mut service_slices: HashMap<ServiceKey, HashMap<String, EndpointSlice>> =
            HashMap::new();

        // Handle EndpointSlice update
        handle_endpointslice_apply(
            &endpointslice,
            &router,
            &mut service_routes,
            &mut service_slices,
        )
        .await
        .expect("Should handle EndpointSlice update");

        // Verify Router now has NEW backends (2 backends: 10.0.1.2 and 10.0.1.3)
        // Make multiple requests to verify distribution across both backends
        let mut backend_ips = std::collections::HashSet::new();
        for i in 0..100 {
            let route_match = router
                .select_backend(
                    HttpMethod::GET,
                    "/api/users",
                    Some(0x0100007f + i), // Vary source IP
                    Some((i % 65535) as u16),
                )
                .expect("Should find backend");

            backend_ips.insert(std::net::Ipv4Addr::from(route_match.backend.ipv4));
        }

        // Should see BOTH new backends (10.0.1.2 and 10.0.1.3), NOT the old one (10.0.1.1)
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 2)),
            "Should have backend 10.0.1.2"
        );
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 3)),
            "Should have backend 10.0.1.3"
        );
        assert!(
            !backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 1)),
            "Should NOT have old backend 10.0.1.1"
        );
    }

    /// RED: Test that multiple EndpointSlices for same Service are aggregated
    ///
    /// Per Kubernetes docs: "there will be at least two EndpointSlice objects"
    /// for a single Service. We must aggregate ALL slices, not just process one.
    ///
    /// Bug scenario:
    /// - Service "api" has 3 EndpointSlices (common when >100 endpoints or IPv4/IPv6)
    /// - Each slice has different backends
    /// - When ONE slice updates, we must include backends from ALL slices
    #[tokio::test]
    async fn test_multiple_endpointslices_per_service_aggregated() {
        // Create Router with initial route
        let router = Arc::new(Router::new());

        let initial_backends = vec![Backend::new(
            u32::from(std::net::Ipv4Addr::new(10, 0, 1, 1)),
            8080,
            100,
        )];

        router
            .add_route(HttpMethod::GET, "/api/users", initial_backends.clone())
            .expect("Should add initial route");

        // Create FIRST EndpointSlice for service "api-service" (slice 1 of 3)
        let mut labels1 = BTreeMap::new();
        labels1.insert(
            "kubernetes.io/service-name".to_string(),
            "api-service".to_string(),
        );

        let endpointslice1 = EndpointSlice {
            metadata: ObjectMeta {
                name: Some("api-service-abc123".to_string()),
                namespace: Some("default".to_string()),
                labels: Some(labels1),
                ..Default::default()
            },
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.2".to_string()],
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
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Create SECOND EndpointSlice for same service (slice 2 of 3)
        let mut labels2 = BTreeMap::new();
        labels2.insert(
            "kubernetes.io/service-name".to_string(),
            "api-service".to_string(),
        );

        let endpointslice2 = EndpointSlice {
            metadata: ObjectMeta {
                name: Some("api-service-def456".to_string()),
                namespace: Some("default".to_string()),
                labels: Some(labels2),
                ..Default::default()
            },
            address_type: "IPv4".to_string(),
            endpoints: vec![
                Endpoint {
                    addresses: vec!["10.0.1.4".to_string()],
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
                    addresses: vec!["10.0.1.5".to_string()],
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
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Create THIRD EndpointSlice for same service (slice 3 of 3)
        let mut labels3 = BTreeMap::new();
        labels3.insert(
            "kubernetes.io/service-name".to_string(),
            "api-service".to_string(),
        );

        let endpointslice3 = EndpointSlice {
            metadata: ObjectMeta {
                name: Some("api-service-ghi789".to_string()),
                namespace: Some("default".to_string()),
                labels: Some(labels3),
                ..Default::default()
            },
            address_type: "IPv4".to_string(),
            endpoints: vec![Endpoint {
                addresses: vec!["10.0.1.6".to_string()],
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
            }],
            ports: Some(vec![EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Track service -> route mapping
        let mut service_routes: HashMap<ServiceKey, Vec<String>> = HashMap::new();
        service_routes.insert(
            "default/api-service".to_string(),
            vec!["/api/users".to_string()],
        );

        // Track EndpointSlices per service
        let mut service_slices: HashMap<ServiceKey, HashMap<String, EndpointSlice>> =
            HashMap::new();

        // Process all 3 EndpointSlices (simulating K8s watcher init)
        handle_endpointslice_apply(
            &endpointslice1,
            &router,
            &mut service_routes,
            &mut service_slices,
        )
        .await
        .expect("Should handle EndpointSlice 1");

        handle_endpointslice_apply(
            &endpointslice2,
            &router,
            &mut service_routes,
            &mut service_slices,
        )
        .await
        .expect("Should handle EndpointSlice 2");

        handle_endpointslice_apply(
            &endpointslice3,
            &router,
            &mut service_routes,
            &mut service_slices,
        )
        .await
        .expect("Should handle EndpointSlice 3");

        // Verify Router has ALL 5 backends from ALL 3 slices
        // (10.0.1.2, 10.0.1.3 from slice1) + (10.0.1.4, 10.0.1.5 from slice2) + (10.0.1.6 from slice3)
        let mut backend_ips = std::collections::HashSet::new();
        for i in 0..1000 {
            let route_match = router
                .select_backend(
                    HttpMethod::GET,
                    "/api/users",
                    Some(0x0100007f + i), // Vary source IP
                    Some((i % 65535) as u16),
                )
                .expect("Should find backend");

            backend_ips.insert(std::net::Ipv4Addr::from(route_match.backend.ipv4));
        }

        // Must see ALL 5 backends (aggregated from 3 slices)
        assert_eq!(
            backend_ips.len(),
            5,
            "Should have all 5 backends from 3 EndpointSlices"
        );
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 2)),
            "Should have backend from slice 1"
        );
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 3)),
            "Should have backend from slice 1"
        );
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 4)),
            "Should have backend from slice 2"
        );
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 5)),
            "Should have backend from slice 2"
        );
        assert!(
            backend_ips.contains(&std::net::Ipv4Addr::new(10, 0, 1, 6)),
            "Should have backend from slice 3"
        );
    }

    #[test]
    fn test_ipv6_addresses_gracefully_skipped() {
        // Test that IPv6 addresses are logged and skipped, not panicking
        use k8s_openapi::api::discovery::v1::{Endpoint, EndpointConditions};

        let endpoint_slice = EndpointSlice {
            address_type: "IPv6".to_string(), // Mixed address type
            endpoints: vec![
                Endpoint {
                    addresses: vec!["2001:db8::1".to_string()], // IPv6 address
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: None,
                        terminating: None,
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["10.0.1.1".to_string()], // IPv4 address (valid)
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: None,
                        terminating: None,
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
                Endpoint {
                    addresses: vec!["::1".to_string()], // IPv6 localhost
                    conditions: Some(EndpointConditions {
                        ready: Some(true),
                        serving: None,
                        terminating: None,
                    }),
                    deprecated_topology: None,
                    hints: None,
                    hostname: None,
                    node_name: None,
                    target_ref: None,
                    zone: None,
                },
            ],
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some("test-slice".to_string()),
                namespace: Some("default".to_string()),
                ..Default::default()
            },
            ports: Some(vec![k8s_openapi::api::discovery::v1::EndpointPort {
                app_protocol: None,
                name: Some("http".to_string()),
                port: Some(8080),
                protocol: Some("TCP".to_string()),
            }]),
        };

        // Parse backends - should only get IPv4 address, IPv6 should be skipped
        let backends = parse_endpointslice_to_backends(&endpoint_slice, 8080);

        // Should have exactly 1 backend (10.0.1.1), IPv6 addresses skipped
        assert_eq!(
            backends.len(),
            1,
            "Should only have IPv4 backend, IPv6 addresses skipped"
        );
        assert_eq!(
            std::net::Ipv4Addr::from(backends[0].ipv4),
            std::net::Ipv4Addr::new(10, 0, 1, 1),
            "Should have IPv4 backend 10.0.1.1"
        );
        assert_eq!(backends[0].port, 8080, "Should have correct port");
    }
}
