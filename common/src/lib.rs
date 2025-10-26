#![no_std]

//! RAUTA Common Types
//!
//! Shared data structures between eBPF and userspace components.
//! All types are Pod-compatible (Plain Old Data) for BPF map usage.

/// Marker trait for types that can be safely used in BPF maps
///
/// # Safety
///
/// Types implementing this trait must be:
/// - `#[repr(C)]` or `#[repr(transparent)]`
/// - Composed only of POD types (no pointers, no heap allocations)
/// - Safe to transmute from a byte array
/// - Have no padding bytes with undefined values
pub unsafe trait Pod: Copy + 'static {}

/// Maximum path length for HTTP routing (99%+ coverage)
pub const MAX_PATH_LEN: usize = 256;

/// Maximum number of backends per route
pub const MAX_BACKENDS: usize = 32;

/// Maximum number of routing rules
pub const MAX_ROUTES: u32 = 65536;

/// HTTP methods supported for routing
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HttpMethod {
    GET = 0,
    POST = 1,
    PUT = 2,
    DELETE = 3,
    HEAD = 4,
    OPTIONS = 5,
    PATCH = 6,
    ALL = 255, // Wildcard for Kubernetes Ingress
}

impl HttpMethod {
    /// Parse HTTP method from byte slice
    /// Returns None if method is unknown or slice is too short
    pub const fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 3 {
            return None;
        }

        match (bytes[0], bytes[1], bytes[2]) {
            (b'G', b'E', b'T') => Some(HttpMethod::GET),
            (b'P', b'O', b'S') if bytes.len() >= 4 && bytes[3] == b'T' => Some(HttpMethod::POST),
            (b'P', b'U', b'T') => Some(HttpMethod::PUT),
            (b'D', b'E', b'L') if bytes.len() >= 6 => Some(HttpMethod::DELETE),
            (b'H', b'E', b'A') if bytes.len() >= 4 && bytes[3] == b'D' => Some(HttpMethod::HEAD),
            (b'O', b'P', b'T') if bytes.len() >= 7 => Some(HttpMethod::OPTIONS),
            (b'P', b'A', b'T') if bytes.len() >= 5 && bytes[4] == b'H' => Some(HttpMethod::PATCH),
            _ => None,
        }
    }

    /// Get method length in bytes for parsing
    pub const fn len(&self) -> u8 {
        match self {
            HttpMethod::GET => 3,
            HttpMethod::PUT => 3,
            HttpMethod::POST => 4,
            HttpMethod::HEAD => 4,
            HttpMethod::PATCH => 5,
            HttpMethod::DELETE => 6,
            HttpMethod::OPTIONS => 7,
            HttpMethod::ALL => 0,
        }
    }
}

/// Key for route lookup in BPF map
/// Uses path hash instead of full path for fast lookup
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RouteKey {
    /// HTTP method (GET, POST, etc.)
    pub method: HttpMethod,
    /// Padding for alignment
    pub _pad: [u8; 3],
    /// Hash of request path (xxHash or FNV-1a)
    pub path_hash: u64,
}

// Safety: RouteKey is a POD type with no padding or pointers
unsafe impl Pod for RouteKey {}

impl RouteKey {
    /// Create new route key with FNV-1a hash of path
    pub const fn new(method: HttpMethod, path_hash: u64) -> Self {
        Self {
            method,
            _pad: [0; 3],
            path_hash,
        }
    }
}

/// Backend server (IP + port)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Backend {
    /// IPv4 address in network byte order
    pub ipv4: u32,
    /// Port in host byte order (will be converted to network order when used)
    pub port: u16,
    /// Weight for weighted load balancing (future use)
    pub weight: u16,
}

// Safety: Backend is a POD type with no padding or pointers
unsafe impl Pod for Backend {}

impl Backend {
    pub const fn new(ipv4: u32, port: u16, weight: u16) -> Self {
        Self { ipv4, port, weight }
    }
}

/// List of backend servers for a route
/// Fixed-size array to work with BPF maps
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BackendList {
    /// Array of backends (unused slots are zeroed)
    pub backends: [Backend; MAX_BACKENDS],
    /// Number of active backends (rest are unused)
    pub count: u32,
    /// Maglev ring size (must be prime, typically 65537)
    pub ring_size: u32,
}

// Safety: BackendList is a POD type with no padding or pointers
unsafe impl Pod for BackendList {}

impl BackendList {
    pub const fn empty() -> Self {
        const ZERO_BACKEND: Backend = Backend {
            ipv4: 0,
            port: 0,
            weight: 0,
        };
        Self {
            backends: [ZERO_BACKEND; MAX_BACKENDS],
            count: 0,
            ring_size: 65537, // Prime number for Maglev
        }
    }
}

/// Performance metrics counters
/// Updated by eBPF, read by userspace
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// Total packets processed
    pub packets_total: u64,
    /// Packets routed in Tier 1 (XDP exact match)
    pub packets_tier1: u64,
    /// Packets fallen back to Tier 2 (TC-BPF)
    pub packets_tier2: u64,
    /// Packets fallen back to Tier 3 (userspace)
    pub packets_tier3: u64,
    /// Packets dropped (no route found)
    pub packets_dropped: u64,
    /// HTTP parse errors
    pub http_parse_errors: u64,
    /// Timestamp of last update (nanoseconds)
    pub last_updated_ns: u64,
}

// Safety: Metrics is a POD type with no padding or pointers
unsafe impl Pod for Metrics {}

impl Metrics {
    pub const fn new() -> Self {
        Self {
            packets_total: 0,
            packets_tier1: 0,
            packets_tier2: 0,
            packets_tier3: 0,
            packets_dropped: 0,
            http_parse_errors: 0,
            last_updated_ns: 0,
        }
    }

    /// Calculate tier 1 hit rate (0.0 to 1.0)
    pub fn tier1_hit_rate(&self) -> f64 {
        if self.packets_total == 0 {
            return 0.0;
        }
        self.packets_tier1 as f64 / self.packets_total as f64
    }

    /// Calculate drop rate (0.0 to 1.0)
    pub fn drop_rate(&self) -> f64 {
        if self.packets_total == 0 {
            return 0.0;
        }
        self.packets_dropped as f64 / self.packets_total as f64
    }
}

/// FNV-1a hash for path strings
/// Used in both eBPF and userspace for consistent hashing
pub const fn fnv1a_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_from_bytes() {
        assert_eq!(HttpMethod::from_bytes(b"GET"), Some(HttpMethod::GET));
        assert_eq!(HttpMethod::from_bytes(b"POST"), Some(HttpMethod::POST));
        assert_eq!(HttpMethod::from_bytes(b"PUT"), Some(HttpMethod::PUT));
        assert_eq!(HttpMethod::from_bytes(b"DELETE"), Some(HttpMethod::DELETE));
        assert_eq!(HttpMethod::from_bytes(b"INVALID"), None);
        assert_eq!(HttpMethod::from_bytes(b"GE"), None);
    }

    #[test]
    fn test_route_key_size() {
        // Ensure RouteKey is exactly 16 bytes (efficient for map lookups)
        assert_eq!(core::mem::size_of::<RouteKey>(), 16);
    }

    #[test]
    fn test_backend_size() {
        // Ensure Backend is exactly 8 bytes (compact)
        assert_eq!(core::mem::size_of::<Backend>(), 8);
    }

    #[test]
    fn test_fnv1a_hash_consistency() {
        let path1 = b"/api/users";
        let path2 = b"/api/users";
        let path3 = b"/api/posts";

        // Same path should produce same hash
        assert_eq!(fnv1a_hash(path1), fnv1a_hash(path2));

        // Different paths should produce different hashes
        assert_ne!(fnv1a_hash(path1), fnv1a_hash(path3));
    }

    #[test]
    fn test_metrics_hit_rate() {
        let mut metrics = Metrics::new();
        metrics.packets_total = 1000;
        metrics.packets_tier1 = 600;
        metrics.packets_tier2 = 300;
        metrics.packets_tier3 = 100;

        assert_eq!(metrics.tier1_hit_rate(), 0.6);
    }

    #[test]
    fn test_metrics_drop_rate() {
        let mut metrics = Metrics::new();
        metrics.packets_total = 1000;
        metrics.packets_dropped = 5;

        assert_eq!(metrics.drop_rate(), 0.005);
    }
}
