use common::{fnv1a_hash, Backend, BackendList, HttpMethod, RouteKey};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// High-level route definition (userspace representation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// HTTP method (GET, POST, etc.)
    pub method: HttpMethod,

    /// Request path (e.g., "/api/users")
    pub path: String,

    /// Host header (optional, for virtual hosting)
    pub host: Option<String>,

    /// Backend servers
    pub backends: Vec<BackendDef>,
}

/// Backend server definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendDef {
    /// IP address
    pub ip: Ipv4Addr,

    /// Port
    pub port: u16,

    /// Weight for weighted load balancing (1-100)
    #[serde(default = "default_weight")]
    pub weight: u16,
}

fn default_weight() -> u16 {
    100
}

impl Route {
    /// Convert to BPF map entry (RouteKey, BackendList)
    pub fn to_bpf_entry(&self) -> (RouteKey, BackendList) {
        // Compute path hash
        let path_hash = fnv1a_hash(self.path.as_bytes());
        let route_key = RouteKey::new(self.method, path_hash);

        // Convert backends
        let mut backend_list = BackendList::empty();
        for (i, backend_def) in self.backends.iter().enumerate() {
            if i >= common::MAX_BACKENDS {
                break;
            }

            backend_list.backends[i] = Backend::new(
                u32::from(backend_def.ip),
                backend_def.port,
                backend_def.weight,
            );
        }
        backend_list.count = self.backends.len().min(common::MAX_BACKENDS) as u32;

        (route_key, backend_list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_to_bpf_entry() {
        let route = Route {
            method: HttpMethod::GET,
            path: "/api/users".to_string(),
            host: None,
            backends: vec![
                BackendDef {
                    ip: Ipv4Addr::new(10, 0, 1, 1),
                    port: 8080,
                    weight: 100,
                },
                BackendDef {
                    ip: Ipv4Addr::new(10, 0, 1, 2),
                    port: 8080,
                    weight: 100,
                },
            ],
        };

        let (key, backends) = route.to_bpf_entry();

        assert_eq!(backends.count, 2);
        assert_eq!(backends.backends[0].port, 8080);
        assert_eq!(backends.backends[1].port, 8080);
    }
}
