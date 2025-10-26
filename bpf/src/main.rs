#![no_std]
#![no_main]

mod forwarding;

use aya_ebpf::{
    bindings::xdp_action,
    macros::{map, xdp},
    maps::{Array, HashMap, LruHashMap, PerCpuArray},
    programs::XdpContext,
};
use core::mem;
use common::{Backend, BackendList, HttpMethod, Metrics, RouteKey, MAGLEV_TABLE_SIZE, MAX_ROUTES};

/// Routing table: RouteKey -> BackendList
/// Hash map for exact path matching (Tier 1)
#[map]
static ROUTES: HashMap<RouteKey, BackendList> =
    HashMap::<RouteKey, BackendList>::with_max_entries(MAX_ROUTES, 0);

/// Flow affinity cache: (client_ip, path_hash) -> backend_index
/// LRU map automatically evicts old flows (Cilium pattern)
#[map]
static FLOW_CACHE: LruHashMap<u64, u32> = LruHashMap::<u64, u32>::with_max_entries(1_000_000, 0);

/// Per-CPU metrics (lock-free performance counters)
/// Using index 0 for global metrics
#[map]
static METRICS: PerCpuArray<Metrics> = PerCpuArray::<Metrics>::with_max_entries(1, 0);

/// Maglev consistent hashing lookup table
/// Array of backend indices for O(1) lookup with minimal disruption
/// Populated by control plane using maglev_build_table()
#[map]
static MAGLEV_TABLE: Array<u32> = Array::<u32>::with_max_entries(MAGLEV_TABLE_SIZE as u32, 0);

/// Maximum HTTP request line length to parse
const MAX_HTTP_LINE: usize = 512;

/// Network protocol constants
const ETH_P_IP: u16 = 0x0800;
const IPPROTO_TCP: u8 = 6;

/// Ethernet header (14 bytes)
#[repr(C)]
struct EthHdr {
    h_dest: [u8; 6],
    h_source: [u8; 6],
    h_proto: u16, // Network byte order
}

/// IPv4 header (20 bytes minimum)
#[repr(C)]
struct Ipv4Hdr {
    _bitfield: u8, // version(4) + ihl(4)
    tos: u8,
    tot_len: u16,
    id: u16,
    frag_off: u16,
    ttl: u8,
    protocol: u8,
    check: u16,
    saddr: u32,
    daddr: u32,
}

/// TCP header (20 bytes minimum)
#[repr(C)]
struct TcpHdr {
    source: u16,
    dest: u16,
    seq: u32,
    ack_seq: u32,
    _bitfield: u16, // doff(4) + reserved(4) + flags(8)
    window: u16,
    check: u16,
    urg_ptr: u16,
}

/// Parse HTTP method from packet data
/// Returns (method, method_length) or None
#[inline(always)]
fn parse_http_method(data: &[u8]) -> Option<(HttpMethod, usize)> {
    if data.len() < 3 {
        return None;
    }

    // Check for common methods (GET, POST, PUT, DELETE)
    match (data[0], data[1], data[2]) {
        (b'G', b'E', b'T') if data.get(3) == Some(&b' ') => Some((HttpMethod::GET, 3)),
        (b'P', b'O', b'S') if data.len() >= 5 && data[3] == b'T' && data[4] == b' ' => {
            Some((HttpMethod::POST, 4))
        }
        (b'P', b'U', b'T') if data.get(3) == Some(&b' ') => Some((HttpMethod::PUT, 3)),
        (b'D', b'E', b'L') if data.len() >= 7 && &data[3..7] == b"ETE " => {
            Some((HttpMethod::DELETE, 6))
        }
        (b'H', b'E', b'A') if data.len() >= 5 && data[3] == b'D' && data[4] == b' ' => {
            Some((HttpMethod::HEAD, 4))
        }
        (b'P', b'A', b'T')
            if data.len() >= 6 && data[3] == b'C' && data[4] == b'H' && data[5] == b' ' =>
        {
            Some((HttpMethod::PATCH, 5))
        }
        (b'O', b'P', b'T') if data.len() >= 8 && &data[3..8] == b"IONS " => {
            Some((HttpMethod::OPTIONS, 7))
        }
        _ => None,
    }
}

/// Extract path from HTTP request and compute FNV-1a hash
/// Returns path_hash or None if parse failed
#[inline(always)]
fn parse_http_path_hash(data: &[u8], method_len: usize) -> Option<u64> {
    if data.len() <= method_len + 1 {
        return None;
    }

    // Skip method + space
    let path_start = method_len + 1;
    let remaining = &data[path_start..];

    // Find end of path (space before HTTP/1.1)
    let mut path_len = 0;
    for (i, &byte) in remaining.iter().enumerate() {
        if byte == b' ' {
            path_len = i;
            break;
        }
        if i >= 256 {
            // Path too long
            return None;
        }
    }

    if path_len == 0 {
        return None;
    }

    // Compute FNV-1a hash of path
    let path = &remaining[..path_len];
    Some(fnv1a_hash(path))
}

/// FNV-1a hash implementation (same as rauta-common)
#[inline(always)]
fn fnv1a_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Select backend using simple hash-based load balancing
/// Uses client IP + path hash for consistent routing
#[inline(always)]
fn select_backend(client_ip: u32, path_hash: u64, backends: &BackendList) -> Option<&Backend> {
    if backends.count == 0 || backends.count > common::MAX_BACKENDS as u32 {
        return None;
    }

    // Create flow key (client_ip combined with path_hash)
    let flow_key = (client_ip as u64) ^ path_hash;

    // Check flow cache first for connection affinity
    if let Some(cached_idx) = unsafe { FLOW_CACHE.get(&flow_key) } {
        let idx = *cached_idx as usize;
        if idx < backends.count as usize {
            return Some(&backends.backends[idx]);
        }
    }

    // Maglev consistent hashing lookup
    // O(1) lookup with minimal disruption (~1/N) when backends change
    let table_idx = (flow_key % MAGLEV_TABLE_SIZE as u64) as u32;

    // Look up backend index in Maglev table
    let backend_idx = match unsafe { MAGLEV_TABLE.get(table_idx) } {
        Some(idx) => *idx as usize,
        None => return None, // Table not initialized
    };

    // Validate index is within current backend list
    if backend_idx >= backends.count as usize {
        return None;
    }

    // Cache the selection for connection affinity
    let _ = unsafe { FLOW_CACHE.insert(&flow_key, &(backend_idx as u32), 0) };

    Some(&backends.backends[backend_idx])
}

/// Increment metrics counter (per-CPU, lock-free)
#[inline(always)]
fn inc_metric<F>(update: F)
where
    F: Fn(&mut Metrics),
{
    if let Some(metrics) = unsafe { METRICS.get_ptr_mut(0) } {
        update(unsafe { &mut *metrics });
    }
}

#[xdp]
pub fn rauta_ingress(ctx: XdpContext) -> u32 {
    match try_rauta_ingress(ctx) {
        Ok(action) => action,
        Err(_) => xdp_action::XDP_PASS,
    }
}

fn try_rauta_ingress(ctx: XdpContext) -> Result<u32, ()> {
    inc_metric(|m| m.packets_total += 1);

    let data = ctx.data();
    let data_end = ctx.data_end();
    let packet_len = data_end - data;

    // Safety: pointers from XdpContext
    let eth = unsafe { ptr_at::<EthHdr>(&ctx, 0)? };

    // Check for IPv4
    let eth_proto = u16::from_be(unsafe { (*eth).h_proto });
    if eth_proto != ETH_P_IP {
        return Ok(xdp_action::XDP_PASS);
    }

    // Parse IPv4 header
    let ip = unsafe { ptr_at::<Ipv4Hdr>(&ctx, mem::size_of::<EthHdr>())? };
    let ip_protocol = unsafe { (*ip).protocol };

    if ip_protocol != IPPROTO_TCP {
        return Ok(xdp_action::XDP_PASS);
    }

    // Get IP header length (IHL field is in lower 4 bits)
    let ihl = unsafe { (*ip)._bitfield & 0x0F };
    let ip_hdr_len = (ihl as usize) * 4;

    // Parse TCP header
    let tcp_offset = mem::size_of::<EthHdr>() + ip_hdr_len;
    let tcp = unsafe { ptr_at::<TcpHdr>(&ctx, tcp_offset)? };

    // Get TCP header length (data offset is in upper 4 bits of _bitfield)
    let tcp_hdr_len = ((unsafe { (*tcp)._bitfield } >> 12) as usize) * 4;

    // HTTP payload starts after TCP header
    let http_offset = tcp_offset + tcp_hdr_len;

    // Bounds check for HTTP parsing
    if http_offset >= packet_len || packet_len - http_offset < 16 {
        // Not enough data for HTTP request line
        return Ok(xdp_action::XDP_PASS);
    }

    // Extract HTTP data into bounded buffer
    let http_data_len = core::cmp::min(packet_len - http_offset, MAX_HTTP_LINE);
    let mut http_buf = [0u8; MAX_HTTP_LINE];

    // Copy HTTP data (verifier-friendly loop)
    for i in 0..http_data_len {
        if http_offset + i >= packet_len {
            break;
        }
        let byte_ptr = (data + http_offset + i) as *const u8;
        if byte_ptr >= data_end as *const u8 {
            break;
        }
        http_buf[i] = unsafe { *byte_ptr };
    }

    let http_slice = &http_buf[..http_data_len];

    // Parse HTTP method
    let (method, method_len) = match parse_http_method(http_slice) {
        Some(parsed) => parsed,
        None => {
            // Not HTTP or unsupported method
            return Ok(xdp_action::XDP_PASS);
        }
    };

    // Parse path and compute hash
    let path_hash = match parse_http_path_hash(http_slice, method_len) {
        Some(hash) => hash,
        None => {
            inc_metric(|m| m.http_parse_errors += 1);
            return Ok(xdp_action::XDP_PASS);
        }
    };

    // Lookup route in routing table
    let route_key = RouteKey::new(method, path_hash);
    let backend_list = match unsafe { ROUTES.get(&route_key) } {
        Some(backends) => backends,
        None => {
            // No route found - fall back to userspace (Tier 3)
            inc_metric(|m| m.packets_tier3 += 1);
            return Ok(xdp_action::XDP_PASS);
        }
    };

    // Select backend
    let client_ip = unsafe { (*ip).saddr };
    let backend = match select_backend(client_ip, path_hash, backend_list) {
        Some(b) => b,
        None => {
            inc_metric(|m| m.packets_dropped += 1);
            return Ok(xdp_action::XDP_DROP);
        }
    };

    // Forward packet to backend
    match forwarding::forward_to_backend(&ctx, backend) {
        Ok(xdp_action::XDP_TX) => {
            // Successfully forwarded in XDP (Tier 1)
            inc_metric(|m| m.packets_tier1 += 1);
            Ok(xdp_action::XDP_TX)
        }
        Ok(xdp_action::XDP_PASS) | Err(_) => {
            // Forward failed - fall back to kernel (Tier 3)
            inc_metric(|m| m.packets_tier3 += 1);
            Ok(xdp_action::XDP_PASS)
        }
        Ok(action) => Ok(action),
    }
}

/// Helper to get typed pointer at offset with bounds checking
#[inline(always)]
unsafe fn ptr_at<T>(ctx: &XdpContext, offset: usize) -> Result<*const T, ()> {
    let data = ctx.data();
    let data_end = ctx.data_end();
    let ptr = (data + offset) as *const T;
    let ptr_end = (ptr as usize + mem::size_of::<T>()) as *const u8;

    if ptr_end as usize > data_end {
        return Err(());
    }

    Ok(ptr)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
