#![no_std]

//! RAUTA Common Types
//!
//! Shared data structures between eBPF and userspace components.
//! All types are Pod-compatible (Plain Old Data) for BPF map usage.

// For userspace: import aya::Pod trait
#[cfg(feature = "aya")]
use aya::Pod;

// For eBPF: No Pod trait needed - types are automatically compatible if #[repr(C)]
#[cfg(feature = "aya-ebpf")]
pub unsafe trait Pod: Copy + 'static {}

// For tests without aya/aya-ebpf: Dummy Pod trait
#[cfg(not(any(feature = "aya", feature = "aya-ebpf")))]
pub unsafe trait Pod: Copy + 'static {}

/// Maximum path length for HTTP routing (99%+ coverage)
pub const MAX_PATH_LEN: usize = 256;

/// Maximum number of backends per route
pub const MAX_BACKENDS: usize = 32;

/// Maximum number of routing rules
pub const MAX_ROUTES: u32 = 65536;

/// Maglev lookup table size (must be prime for good distribution)
/// Using 65537 (next prime after 65536) as recommended by Google's Maglev paper
pub const MAGLEV_TABLE_SIZE: usize = 65537;

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
        if bytes.len() < 4 {
            // Minimum: "GET " = 4 bytes
            return None;
        }

        match (bytes[0], bytes[1], bytes[2]) {
            (b'G', b'E', b'T') if bytes[3] == b' ' => Some(HttpMethod::GET),
            (b'P', b'O', b'S') if bytes.len() >= 5 && bytes[3] == b'T' && bytes[4] == b' ' => {
                Some(HttpMethod::POST)
            }
            (b'P', b'U', b'T') if bytes[3] == b' ' => Some(HttpMethod::PUT),
            (b'D', b'E', b'L')
                if bytes.len() >= 7
                    && bytes[3] == b'E'
                    && bytes[4] == b'T'
                    && bytes[5] == b'E'
                    && bytes[6] == b' ' =>
            {
                Some(HttpMethod::DELETE)
            }
            (b'H', b'E', b'A') if bytes.len() >= 5 && bytes[3] == b'D' && bytes[4] == b' ' => {
                Some(HttpMethod::HEAD)
            }
            (b'O', b'P', b'T')
                if bytes.len() >= 8
                    && bytes[3] == b'I'
                    && bytes[4] == b'O'
                    && bytes[5] == b'N'
                    && bytes[6] == b'S'
                    && bytes[7] == b' ' =>
            {
                Some(HttpMethod::OPTIONS)
            }
            (b'P', b'A', b'T')
                if bytes.len() >= 6
                    && bytes[3] == b'C'
                    && bytes[4] == b'H'
                    && bytes[5] == b' ' =>
            {
                Some(HttpMethod::PATCH)
            }
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

/// Maglev Consistent Hashing Implementation
///
/// Based on Google's Maglev paper: https://research.google/pubs/pub44824/
///
/// Maglev provides O(1) lookup with minimal disruption (~1/N) when backends change.

#[cfg(not(target_arch = "bpf"))]
extern crate alloc;
#[cfg(not(target_arch = "bpf"))]
use alloc::vec;
#[cfg(not(target_arch = "bpf"))]
use alloc::vec::Vec;

/// Build Maglev lookup table for a set of backends
///
/// Returns a table of size MAGLEV_TABLE_SIZE where each entry maps to a backend index.
/// The algorithm ensures:
/// - Even distribution across backends
/// - Minimal disruption when backends change (~1/N)
/// - Deterministic results for same backend set
#[cfg(not(target_arch = "bpf"))]
pub fn maglev_build_table(backends: &[Backend]) -> Vec<Option<u32>> {
    if backends.is_empty() {
        return vec![None; MAGLEV_TABLE_SIZE];
    }

    // Initialize table with None
    let mut table = vec![None; MAGLEV_TABLE_SIZE];

    // Generate permutations for each backend
    let mut permutations = Vec::with_capacity(backends.len());
    for (i, backend) in backends.iter().enumerate() {
        permutations.push(generate_permutation(backend, i as u32));
    }

    // Fill the table
    let mut filled = 0;
    let mut n = 0;

    // Continue until table is full
    while filled < MAGLEV_TABLE_SIZE {
        for (backend_idx, perm) in permutations.iter().enumerate() {
            let c = perm[n % MAGLEV_TABLE_SIZE];

            // Try to claim this slot
            if table[c].is_none() {
                table[c] = Some(backend_idx as u32);
                filled += 1;

                if filled == MAGLEV_TABLE_SIZE {
                    break;
                }
            }
        }
        n += 1;
    }

    table
}

/// Generate permutation for a backend using Maglev's double hashing
#[cfg(not(target_arch = "bpf"))]
fn generate_permutation(backend: &Backend, backend_idx: u32) -> Vec<usize> {
    // Generate two hash values for offset and skip
    // Use backend IP + port as key, backend_idx as additional seed to avoid collisions
    let key = ((backend.ipv4 as u64) << 16) | (backend.port as u64);

    let offset = (hash_backend(key, backend_idx as u64) % MAGLEV_TABLE_SIZE as u64) as usize;
    let skip = ((hash_backend(key, backend_idx as u64 + 1) % (MAGLEV_TABLE_SIZE as u64 - 1)) + 1) as usize;

    // Generate permutation
    let mut perm = Vec::with_capacity(MAGLEV_TABLE_SIZE);
    for j in 0..MAGLEV_TABLE_SIZE {
        perm.push((offset + j * skip) % MAGLEV_TABLE_SIZE);
    }

    perm
}

/// Hash function for Maglev permutation generation
#[cfg(not(target_arch = "bpf"))]
fn hash_backend(key: u64, seed: u64) -> u64 {
    // FNV-1a hash with seed
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET ^ seed;

    // Hash the key bytes
    for i in 0..8 {
        let byte = ((key >> (i * 8)) & 0xFF) as u8;
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    hash
}

/// Lookup backend index for a given flow key using Maglev table
///
/// This is the fast-path function used in XDP.
/// Takes O(1) time.
#[inline(always)]
pub fn maglev_lookup(flow_key: u64, table: &[Option<u32>]) -> Option<u32> {
    if table.is_empty() {
        return None;
    }

    // Hash the flow key and look up in table
    let idx = (flow_key % table.len() as u64) as usize;
    table.get(idx).copied().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_from_bytes() {
        // HTTP methods must be followed by a space (HTTP/1.1 spec)
        assert_eq!(HttpMethod::from_bytes(b"GET /"), Some(HttpMethod::GET));
        assert_eq!(HttpMethod::from_bytes(b"POST /"), Some(HttpMethod::POST));
        assert_eq!(HttpMethod::from_bytes(b"PUT /"), Some(HttpMethod::PUT));
        assert_eq!(HttpMethod::from_bytes(b"DELETE /"), Some(HttpMethod::DELETE));

        // Invalid methods should return None
        assert_eq!(HttpMethod::from_bytes(b"INVALID"), None);
        assert_eq!(HttpMethod::from_bytes(b"GE"), None);
        assert_eq!(HttpMethod::from_bytes(b"GET"), None); // Missing space!
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
