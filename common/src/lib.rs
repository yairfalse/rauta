#![no_std]

//! RAUTA Common Types
//!
//! Core data structures for HTTP routing, load balancing, and backend management.
//! All types are no_std compatible and memory-efficient for high-performance routing.

/// Maximum path length for HTTP routing (99%+ coverage)
pub const MAX_PATH_LEN: usize = 256;

/// Maximum number of backends per route
pub const MAX_BACKENDS: usize = 32;

/// Maximum number of routing rules
pub const MAX_ROUTES: u32 = 65536;

/// Maglev lookup table size (must be prime for good distribution)
/// Using 65537 (next prime after 65536) as recommended by Google's Maglev paper
pub const MAGLEV_TABLE_SIZE: usize = 65537;

/// Compact Maglev table size for embedded per-route tables
/// Using 4099 (prime) for memory efficiency: 4KB per route instead of 262KB
/// With MAX_BACKENDS=32, this still provides excellent distribution
pub const COMPACT_MAGLEV_SIZE: usize = 4099;

/// HTTP methods supported for routing
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
                if bytes.len() >= 6 && bytes[3] == b'C' && bytes[4] == b'H' && bytes[5] == b' ' =>
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

    /// Check if method is empty (always false for valid HTTP methods)
    pub const fn is_empty(&self) -> bool {
        false
    }
}

/// Key for route lookup
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
///
/// Supports both IPv4 and IPv6 addresses. IPv4 addresses are stored directly
/// in the first 4 bytes of the `ip` array.
///
/// **Memory Layout**:
/// - `ip`: 16 bytes (IPv6 address, or IPv4 in first 4 bytes)
/// - `port`: 2 bytes
/// - `weight`: 2 bytes
/// - `is_ipv6`: 1 byte (flag)
/// - `_pad`: 3 bytes (alignment padding)
/// - **Total**: 24 bytes
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Backend {
    /// IP address (16 bytes)
    /// - IPv4: stored in first 4 bytes (network byte order)
    /// - IPv6: full 16 bytes (network byte order)
    ip: [u8; 16],

    /// Port in host byte order
    pub port: u16,

    /// Weight for weighted load balancing (1-65535)
    pub weight: u16,

    /// True if this is an IPv6 address
    is_ipv6: bool,

    /// Padding for 8-byte alignment
    _pad: [u8; 3],
}

impl Backend {
    /// Create Backend from IPv4 address
    pub fn from_ipv4(ipv4: core::net::Ipv4Addr, port: u16, weight: u16) -> Self {
        let mut ip = [0u8; 16];
        ip[0..4].copy_from_slice(&ipv4.octets());

        Self {
            ip,
            port,
            weight,
            is_ipv6: false,
            _pad: [0; 3],
        }
    }

    /// Create Backend from IPv6 address
    pub fn from_ipv6(ipv6: core::net::Ipv6Addr, port: u16, weight: u16) -> Self {
        Self {
            ip: ipv6.octets(),
            port,
            weight,
            is_ipv6: true,
            _pad: [0; 3],
        }
    }

    /// Legacy constructor for compatibility (IPv4 only)
    ///
    /// **Deprecated**: Use `from_ipv4` instead for clarity
    pub const fn new(ipv4: u32, port: u16, weight: u16) -> Self {
        let bytes = ipv4.to_be_bytes();
        let mut ip = [0u8; 16];
        ip[0] = bytes[0];
        ip[1] = bytes[1];
        ip[2] = bytes[2];
        ip[3] = bytes[3];

        Self {
            ip,
            port,
            weight,
            is_ipv6: false,
            _pad: [0; 3],
        }
    }

    /// Check if this backend uses IPv6
    pub fn is_ipv6(&self) -> bool {
        self.is_ipv6
    }

    /// Get IPv4 address (returns None if this is IPv6)
    pub fn as_ipv4(&self) -> Option<core::net::Ipv4Addr> {
        if self.is_ipv6 {
            return None;
        }

        Some(core::net::Ipv4Addr::new(
            self.ip[0], self.ip[1], self.ip[2], self.ip[3],
        ))
    }

    /// Get IPv6 address (returns None if this is IPv4)
    pub fn as_ipv6(&self) -> Option<core::net::Ipv6Addr> {
        if !self.is_ipv6 {
            return None;
        }

        Some(core::net::Ipv6Addr::from(self.ip))
    }

    /// Get IPv4 address as u32 (for logging and legacy code)
    ///
    /// **Panics** if this is an IPv6 backend. Use only in contexts where IPv4 is guaranteed.
    ///
    /// **Note**: This helper reduces code duplication from the common pattern:
    /// `u32::from(backend.as_ipv4().unwrap())`
    #[allow(clippy::expect_used)]
    pub fn ipv4_as_u32(&self) -> u32 {
        u32::from(self.as_ipv4().expect("Backend must be IPv4"))
    }

    /// Convert Backend to SocketAddr (for TCP connections)
    ///
    /// Returns a proper SocketAddr for both IPv4 and IPv6 backends.
    /// Use this for TcpStream::connect() calls.
    pub fn to_socket_addr(&self) -> core::net::SocketAddr {
        if self.is_ipv6 {
            core::net::SocketAddr::V6(core::net::SocketAddrV6::new(
                core::net::Ipv6Addr::from(self.ip),
                self.port,
                0, // flowinfo
                0, // scope_id
            ))
        } else {
            // Safe: is_ipv6 == false means this must be IPv4
            core::net::SocketAddr::V4(core::net::SocketAddrV4::new(
                core::net::Ipv4Addr::new(self.ip[0], self.ip[1], self.ip[2], self.ip[3]),
                self.port,
            ))
        }
    }

    /// Convert IPv4 backend to IPv4-mapped IPv6 address (::ffff:x.x.x.x)
    pub fn to_ipv4_mapped(&self) -> core::net::Ipv6Addr {
        if self.is_ipv6 {
            // Already IPv6, return as-is
            core::net::Ipv6Addr::from(self.ip)
        } else {
            // IPv4 - convert to IPv4-mapped IPv6
            // Safe: is_ipv6 == false means this must be IPv4
            let ipv4 = core::net::Ipv4Addr::new(self.ip[0], self.ip[1], self.ip[2], self.ip[3]);
            ipv4.to_ipv6_mapped()
        }
    }

    /// Get the raw IP bytes (for Maglev hashing)
    pub fn ip_bytes(&self) -> &[u8; 16] {
        &self.ip
    }
}

impl core::fmt::Display for Backend {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_ipv6 {
            // IPv6 format: [2001:db8::1]:8080
            let ipv6 = core::net::Ipv6Addr::from(self.ip);
            write!(f, "[{}]:{}", ipv6, self.port)
        } else {
            // IPv4 format: 192.168.1.100:8080
            write!(
                f,
                "{}.{}.{}.{}:{}",
                self.ip[0], self.ip[1], self.ip[2], self.ip[3], self.port
            )
        }
    }
}

/// List of backend servers for a route
/// Fixed-size array for backend storage
///
/// Memory layout:
/// - backends: 32 × 8 bytes = 256 bytes
/// - count: 4 bytes
/// - Total: 260 bytes ✅ Memory efficient at 260 bytes
///
/// Note: Maglev tables stored separately in MAGLEV_TABLES map
/// keeping the table separate (4KB)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BackendList {
    /// Array of backends (unused slots are zeroed)
    pub backends: [Backend; MAX_BACKENDS],
    /// Number of active backends (rest are unused)
    pub count: u32,
    /// Padding for alignment
    pub _pad: u32,
}

impl BackendList {
    pub const fn empty() -> Self {
        const ZERO_BACKEND: Backend = Backend {
            ip: [0u8; 16],
            port: 0,
            weight: 0,
            is_ipv6: false,
            _pad: [0; 3],
        };
        Self {
            backends: [ZERO_BACKEND; MAX_BACKENDS],
            count: 0,
            _pad: 0,
        }
    }
}

/// Compact Maglev lookup table (stored separately for memory efficiency)
/// 4KB per route - stored in MAGLEV_TABLES map indexed by path_hash
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CompactMaglevTable {
    /// Backend indices (0..MAX_BACKENDS)
    pub table: [u8; COMPACT_MAGLEV_SIZE],
}

impl CompactMaglevTable {
    pub const fn empty() -> Self {
        Self {
            table: [0u8; COMPACT_MAGLEV_SIZE],
        }
    }
}

/// Performance metrics counters
/// Routing performance metrics
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// Total packets processed
    pub packets_total: u64,
    /// Packets routed via direct lookup
    pub packets_tier1: u64,
    /// Packets using fallback path
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

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a hash for path strings
/// FNV-1a hash function for consistent hashing
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
extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

/// Build Maglev lookup table for a set of backends
///
/// Returns a table of size MAGLEV_TABLE_SIZE where each entry maps to a backend index.
/// The algorithm ensures:
/// - Even distribution across backends
/// - Minimal disruption when backends change (~1/N)
/// - Deterministic results for same backend set
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
fn generate_permutation(backend: &Backend, backend_idx: u32) -> Vec<usize> {
    // Generate two hash values for offset and skip
    // Hash full 16-byte IP + port for both IPv4 and IPv6 support
    let mut hasher_bytes = [0u8; 18];
    hasher_bytes[0..16].copy_from_slice(backend.ip_bytes());
    hasher_bytes[16..18].copy_from_slice(&backend.port.to_be_bytes());

    // Use FNV-1a hash over the full IP+port
    let key = fnv1a_hash(&hasher_bytes);

    let offset = (hash_backend(key, backend_idx as u64) % (MAGLEV_TABLE_SIZE as u64)) as usize;
    let skip =
        ((hash_backend(key, backend_idx as u64 + 1) % (MAGLEV_TABLE_SIZE as u64 - 1)) + 1) as usize;

    // Generate permutation
    let mut perm = Vec::with_capacity(MAGLEV_TABLE_SIZE);
    for j in 0..MAGLEV_TABLE_SIZE {
        perm.push((offset + j * skip) % MAGLEV_TABLE_SIZE);
    }

    perm
}

/// Hash function for Maglev permutation generation
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
/// Fast lookup function using Maglev consistent hashing.
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

/// Build compact Maglev lookup table for embedded per-route tables
///
/// Returns a table of size COMPACT_MAGLEV_SIZE (4099) with u8 backend indices.
/// This is memory-efficient: 4KB per route instead of 262KB.
///
/// # Arguments
/// * `backends` - Slice of backends for this route (max 32)
///
/// # Returns
/// Vec of u8 backend indices (each < MAX_BACKENDS)
pub fn maglev_build_compact_table(backends: &[Backend]) -> Vec<u8> {
    if backends.is_empty() {
        return vec![0u8; COMPACT_MAGLEV_SIZE];
    }

    // Validate backend count
    assert!(
        backends.len() <= MAX_BACKENDS,
        "Cannot build compact table for {} backends (max {})",
        backends.len(),
        MAX_BACKENDS
    );

    // Initialize table with 0 (will always have at least 1 backend)
    let mut table = vec![0u8; COMPACT_MAGLEV_SIZE];

    // Generate permutations for each backend (weighted)
    // For weighted load balancing: repeat each backend based on its weight
    // Example: weight=90 → 9 permutations, weight=10 → 1 permutation
    let mut permutations = Vec::new();
    for (i, backend) in backends.iter().enumerate() {
        // Normalize weight: weight / 10 (so weight=100 → 10 reps)
        // Minimum 1 repetition even if weight=0
        let repetitions = ((backend.weight / 10).max(1)) as usize;

        for rep in 0..repetitions {
            // Use unique index for each repetition to get different permutations
            // This ensures weighted backends get different hash patterns
            let unique_idx = (i * 100 + rep) as u32; // Spread out indices
            permutations.push((i as u8, generate_permutation_compact(backend, unique_idx)));
        }
    }

    // Track which slots are filled
    let mut filled = vec![false; COMPACT_MAGLEV_SIZE];
    let mut filled_count = 0;

    // Fill the table using Maglev algorithm
    let mut n = 0;
    while filled_count < COMPACT_MAGLEV_SIZE {
        for (backend_idx, perm) in permutations.iter() {
            let c = perm[n % COMPACT_MAGLEV_SIZE];

            // Try to claim this slot
            if !filled[c] {
                table[c] = *backend_idx; // Use backend index from tuple
                filled[c] = true;
                filled_count += 1;

                if filled_count == COMPACT_MAGLEV_SIZE {
                    break;
                }
            }
        }
        n += 1;
    }

    table
}

/// Generate permutation for compact table using Maglev's double hashing
fn generate_permutation_compact(backend: &Backend, backend_idx: u32) -> Vec<usize> {
    // Generate two hash values for offset and skip
    // Hash full 16-byte IP + port for both IPv4 and IPv6 support
    let mut hasher_bytes = [0u8; 18];
    hasher_bytes[0..16].copy_from_slice(backend.ip_bytes());
    hasher_bytes[16..18].copy_from_slice(&backend.port.to_be_bytes());

    // Use FNV-1a hash over the full IP+port
    let key = fnv1a_hash(&hasher_bytes);

    let offset = (hash_backend(key, backend_idx as u64) % (COMPACT_MAGLEV_SIZE as u64)) as usize;
    let skip = ((hash_backend(key, backend_idx as u64 + 1) % (COMPACT_MAGLEV_SIZE as u64 - 1)) + 1)
        as usize;

    // Generate permutation
    let mut perm = Vec::with_capacity(COMPACT_MAGLEV_SIZE);
    for j in 0..COMPACT_MAGLEV_SIZE {
        perm.push((offset + j * skip) % COMPACT_MAGLEV_SIZE);
    }

    perm
}

/// Lookup backend index in compact Maglev table
///
/// Fast lookup function for per-route Maglev tables.
/// Takes O(1) time.
#[inline(always)]
pub fn maglev_lookup_compact(flow_key: u64, table: &[u8]) -> u8 {
    if table.is_empty() {
        return 0;
    }

    // Hash the flow key and look up in table
    let idx = (flow_key % (COMPACT_MAGLEV_SIZE as u64)) as usize;
    table[idx]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::format;

    #[test]
    fn test_http_method_from_bytes() {
        // HTTP methods must be followed by a space (HTTP/1.1 spec)
        assert_eq!(HttpMethod::from_bytes(b"GET /"), Some(HttpMethod::GET));
        assert_eq!(HttpMethod::from_bytes(b"POST /"), Some(HttpMethod::POST));
        assert_eq!(HttpMethod::from_bytes(b"PUT /"), Some(HttpMethod::PUT));
        assert_eq!(
            HttpMethod::from_bytes(b"DELETE /"),
            Some(HttpMethod::DELETE)
        );

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
        // Backend is 24 bytes after IPv6 support (16 bytes IP + 2 port + 2 weight + 1 is_ipv6 + 3 padding)
        assert_eq!(core::mem::size_of::<Backend>(), 24);
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

    // ============================================================================
    // RED: IPv6 Backend Support Tests
    // These tests will FAIL until Backend struct is updated to support IPv6
    // ============================================================================

    #[test]
    fn test_backend_ipv4_construction() {
        use std::net::Ipv4Addr;

        // RED: This test will fail because Backend::from_ipv4 doesn't exist yet
        let backend = Backend::from_ipv4(Ipv4Addr::new(192, 168, 1, 100), 8080, 100);

        assert_eq!(backend.port, 8080);
        assert_eq!(backend.weight, 100);
        assert!(!backend.is_ipv6());

        // Should be able to convert back to IPv4
        assert_eq!(backend.as_ipv4().unwrap(), Ipv4Addr::new(192, 168, 1, 100));
    }

    #[test]
    fn test_backend_ipv6_construction() {
        use std::net::Ipv6Addr;

        // RED: This test will fail because Backend::from_ipv6 doesn't exist yet
        let backend = Backend::from_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 8080, 100);

        assert_eq!(backend.port, 8080);
        assert_eq!(backend.weight, 100);
        assert!(backend.is_ipv6());

        // Should be able to convert back to IPv6
        assert_eq!(
            backend.as_ipv6().unwrap(),
            Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)
        );
    }

    #[test]
    fn test_backend_ipv4_mapped_to_ipv6() {
        use std::net::Ipv4Addr;

        // RED: Test IPv4-mapped IPv6 address (::ffff:192.168.1.100)
        let ipv4 = Ipv4Addr::new(192, 168, 1, 100);
        let backend_v4 = Backend::from_ipv4(ipv4, 8080, 100);

        // IPv4 addresses should NOT be treated as IPv6
        assert!(!backend_v4.is_ipv6());

        // Should be able to convert to IPv4-mapped IPv6
        let ipv6_mapped = backend_v4.to_ipv4_mapped();
        assert_eq!(ipv6_mapped, ipv4.to_ipv6_mapped());
    }

    #[test]
    fn test_backend_equality_ipv4() {
        use std::net::Ipv4Addr;

        // RED: Test that two IPv4 backends with same IP are equal
        let backend1 = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 1), 8080, 100);
        let backend2 = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 1), 8080, 100);
        let backend3 = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 2), 8080, 100);

        assert_eq!(backend1, backend2);
        assert_ne!(backend1, backend3);
    }

    #[test]
    fn test_backend_equality_ipv6() {
        use std::net::Ipv6Addr;

        // RED: Test that two IPv6 backends with same IP are equal
        let backend1 =
            Backend::from_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 8080, 100);
        let backend2 =
            Backend::from_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 8080, 100);
        let backend3 =
            Backend::from_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2), 8080, 100);

        assert_eq!(backend1, backend2);
        assert_ne!(backend1, backend3);
    }

    #[test]
    fn test_backend_hash_consistency() {
        use std::collections::HashSet;
        use std::net::Ipv4Addr;

        // RED: Test that Backend can be used in HashSet
        let mut backends = HashSet::new();

        let backend1 = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 1), 8080, 100);
        let backend2 = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 1), 8080, 100);
        let backend3 = Backend::from_ipv4(Ipv4Addr::new(10, 0, 1, 2), 8080, 100);

        backends.insert(backend1);
        backends.insert(backend2); // Should not add duplicate

        assert_eq!(backends.len(), 1);

        backends.insert(backend3);
        assert_eq!(backends.len(), 2);
    }

    #[test]
    fn test_backend_size_after_ipv6() {
        // RED: Backend should be 24 bytes after IPv6 support
        // Current: 8 bytes (u32 + u16 + u16)
        // New: 24 bytes ([u8; 16] + u16 + u16 + bool + [u8; 3])
        assert_eq!(core::mem::size_of::<Backend>(), 24);
    }

    #[test]
    fn test_backend_list_size_after_ipv6() {
        // RED: BackendList size should be updated
        // 32 backends × 24 bytes = 768 bytes
        // + 4 bytes (count) + 4 bytes (_pad) = 776 bytes
        assert_eq!(core::mem::size_of::<BackendList>(), 776);
    }

    #[test]
    fn test_backend_display_ipv4() {
        use std::net::Ipv4Addr;

        // RED: Test Display trait for IPv4 backends
        let backend = Backend::from_ipv4(Ipv4Addr::new(192, 168, 1, 100), 8080, 100);
        let display = format!("{}", backend);

        assert_eq!(display, "192.168.1.100:8080");
    }

    #[test]
    fn test_backend_display_ipv6() {
        use std::net::Ipv6Addr;

        // RED: Test Display trait for IPv6 backends
        let backend = Backend::from_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 8080, 100);
        let display = format!("{}", backend);

        assert_eq!(display, "[2001:db8::1]:8080");
    }

    #[test]
    fn test_backend_to_socket_addr_ipv4() {
        use std::net::{Ipv4Addr, SocketAddr};

        // Test converting IPv4 Backend to SocketAddr
        let backend = Backend::from_ipv4(Ipv4Addr::new(192, 168, 1, 100), 8080, 100);
        let socket_addr = backend.to_socket_addr();

        match socket_addr {
            SocketAddr::V4(addr) => {
                assert_eq!(*addr.ip(), Ipv4Addr::new(192, 168, 1, 100));
                assert_eq!(addr.port(), 8080);
            }
            SocketAddr::V6(_) => panic!("Expected IPv4 SocketAddr"),
        }
    }

    #[test]
    fn test_backend_to_socket_addr_ipv6() {
        use std::net::{Ipv6Addr, SocketAddr};

        // Test converting IPv6 Backend to SocketAddr
        let backend = Backend::from_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 8080, 100);
        let socket_addr = backend.to_socket_addr();

        match socket_addr {
            SocketAddr::V6(addr) => {
                assert_eq!(*addr.ip(), Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
                assert_eq!(addr.port(), 8080);
            }
            SocketAddr::V4(_) => panic!("Expected IPv6 SocketAddr"),
        }
    }
}
