/// Packet forwarding module
///
/// XDP_TX hairpin NAT implementation for L7 HTTP load balancing.
use aya_ebpf::bindings::xdp_action;
use aya_ebpf::programs::XdpContext;
use common::Backend;
use core::mem;

/// Ethernet header (14 bytes)
#[repr(C)]
struct EthHdr {
    h_dest: [u8; 6],
    h_source: [u8; 6],
    h_proto: u16,
}

/// IPv4 header (20 bytes minimum)
#[repr(C)]
struct Ipv4Hdr {
    _bitfield: u8,
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
    _bitfield: u16,
    window: u16,
    check: u16,
    urg_ptr: u16,
}

/// Forward packet to backend using XDP_TX
///
/// Strategy: Hairpin NAT (rewrite dst MAC/IP/port and send back on same interface)
///
/// Steps:
/// 1. Rewrite IP dst → backend IP
/// 2. Rewrite TCP dst port → backend port
/// 3. Recalculate IP checksum
/// 4. Recalculate TCP checksum (simplified - zero out for now)
/// 5. Swap src/dst MAC (hairpin - send back where it came from)
/// 6. Return XDP_TX to send packet back
///
/// Returns:
/// - XDP_TX: Packet forwarded successfully
/// - XDP_PASS: Fall back to kernel (fragmentation, errors)
#[inline(always)]
pub fn forward_to_backend(ctx: &XdpContext, backend: &Backend) -> Result<u32, ()> {
    let data = ctx.data();
    let data_end = ctx.data_end();

    // Parse Ethernet header
    let eth = unsafe { ptr_at::<EthHdr>(ctx, 0)? };

    // Parse IPv4 header
    let ip = unsafe { ptr_at_mut::<Ipv4Hdr>(ctx, mem::size_of::<EthHdr>())? };
    let ip_hdr_len = (unsafe { (*ip)._bitfield } & 0x0F) as usize * 4;

    // Check for fragmentation - pass fragmented packets to kernel
    let frag_off = u16::from_be(unsafe { (*ip).frag_off });
    let is_fragmented = (frag_off & 0x1FFF) != 0 || (frag_off & 0x2000) != 0; // Offset or MF flag
    if is_fragmented {
        return Ok(xdp_action::XDP_PASS);
    }

    // Parse TCP header
    let tcp_offset = mem::size_of::<EthHdr>() + ip_hdr_len;
    let tcp = unsafe { ptr_at_mut::<TcpHdr>(ctx, tcp_offset)? };

    // Step 1: Rewrite destination IP
    let old_daddr = unsafe { (*ip).daddr };
    let new_daddr = backend.ipv4;
    unsafe {
        (*ip).daddr = new_daddr;
    }

    // Step 2: Rewrite destination port
    let old_dport = unsafe { (*tcp).dest };
    let new_dport = u16::to_be(backend.port);
    unsafe {
        (*tcp).dest = new_dport;
    }

    // Step 3: Recalculate IP checksum
    // Zero out old checksum first
    unsafe {
        (*ip).check = 0;
    }
    let ip_csum = calculate_ip_checksum(ip, ip_hdr_len)?;
    unsafe {
        (*ip).check = ip_csum;
    }

    // Step 4: Recalculate TCP checksum
    // For simplicity, we'll use incremental checksum update
    unsafe {
        let old_tcp_check = (*tcp).check;
        let tcp_check =
            update_tcp_checksum(old_tcp_check, old_daddr, new_daddr, old_dport, new_dport);
        (*tcp).check = tcp_check;
    }

    // Step 5: Swap MAC addresses for hairpin (send back on same interface)
    // In production, we'd do ARP lookup for backend MAC
    // For now, swap src/dst to send back to sender (who will forward to backend)
    unsafe {
        let eth_mut = eth as *mut EthHdr;
        let temp_mac = (*eth_mut).h_source;
        (*eth_mut).h_source = (*eth_mut).h_dest;
        (*eth_mut).h_dest = temp_mac;
    }

    // Step 6: Return XDP_TX to transmit modified packet
    Ok(xdp_action::XDP_TX)
}

/// Calculate IPv4 header checksum
#[inline(always)]
fn calculate_ip_checksum(ip: *const Ipv4Hdr, hdr_len: usize) -> Result<u16, ()> {
    let mut sum: u32 = 0;

    // Treat IP header as array of u16 values
    let ip_bytes = ip as *const u16;

    // Sum all 16-bit words (unrolled for common case: 20 bytes = 10 words)
    #[allow(clippy::needless_range_loop)]
    for i in 0..10 {
        if i * 2 >= hdr_len {
            break;
        }
        let word = unsafe { core::ptr::read_unaligned(ip_bytes.add(i)) };
        sum += u16::from_be(word) as u32;
    }

    // Fold 32-bit sum to 16 bits
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    // One's complement
    Ok(!sum as u16)
}

/// Update TCP checksum incrementally (RFC 1624)
///
/// When we change IP addresses and ports, we can update checksum without
/// recalculating from scratch.
#[inline(always)]
fn update_tcp_checksum(
    old_check: u16,
    old_ip: u32,
    new_ip: u32,
    old_port: u16,
    new_port: u16,
) -> u16 {
    // Convert checksum to 32-bit for calculation
    let mut sum = (!u16::from_be(old_check)) as u32;

    // Subtract old IP (as two 16-bit words)
    sum = checksum_sub(sum, (old_ip >> 16) as u16);
    sum = checksum_sub(sum, (old_ip & 0xFFFF) as u16);

    // Add new IP (as two 16-bit words)
    sum = checksum_add(sum, (new_ip >> 16) as u16);
    sum = checksum_add(sum, (new_ip & 0xFFFF) as u16);

    // Subtract old port
    sum = checksum_sub(sum, u16::from_be(old_port));

    // Add new port
    sum = checksum_add(sum, u16::from_be(new_port));

    // Fold carries
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    // One's complement and convert to network byte order
    u16::to_be(!sum as u16)
}

/// Add value to checksum with carry folding
#[inline(always)]
fn checksum_add(sum: u32, val: u16) -> u32 {
    let new_sum = sum + val as u32;
    // Fold carry
    (new_sum & 0xFFFF) + (new_sum >> 16)
}

/// Subtract value from checksum with borrow handling
#[inline(always)]
fn checksum_sub(sum: u32, val: u16) -> u32 {
    checksum_add(sum, !val)
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

/// Helper to get mutable typed pointer at offset with bounds checking
#[inline(always)]
unsafe fn ptr_at_mut<T>(ctx: &XdpContext, offset: usize) -> Result<*mut T, ()> {
    let data = ctx.data();
    let data_end = ctx.data_end();
    let ptr = (data + offset) as *mut T;
    let ptr_end = (ptr as usize + mem::size_of::<T>()) as *const u8;

    if ptr_end as usize > data_end {
        return Err(());
    }

    Ok(ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These are pseudo-tests since we can't easily test XDP programs in Rust
    // Real tests would use BPF selftests framework

    #[test]
    #[ignore] // Will pass once forwarding is implemented
    fn test_forward_to_backend_returns_xdp_tx() {
        // RED: This test will fail until forwarding is implemented
        //
        // Expected behavior:
        // - forward_to_backend() should return XDP_TX when successful
        //
        // Current behavior:
        // - Returns XDP_PASS (fallback)
        //
        // This test documents the expected behavior.
        // Once implemented, remove #[ignore] attribute.

        // We can't easily create an XdpContext here, but we document expectations:
        // let ctx = mock_xdp_context();
        // let backend = Backend::new(0x0100007f, 8080, 100); // 127.0.0.1:8080
        //
        // let result = forward_to_backend(&ctx, &backend).unwrap();
        // assert_eq!(result, xdp_action::XDP_TX);

        panic!("Test not yet implemented - waiting for forward_to_backend implementation");
    }

    #[test]
    #[ignore]
    fn test_forward_rewrites_destination_ip() {
        // RED: Verify IP rewriting
        //
        // Given:
        // - Packet with dst IP 10.0.0.1 (VIP)
        // - Backend IP 10.0.1.5
        //
        // Expected:
        // - Packet dst IP rewritten to 10.0.1.5
        //
        // Verification:
        // - Parse packet after forward_to_backend()
        // - Check IP header dst field

        panic!("Test not yet implemented");
    }

    #[test]
    #[ignore]
    fn test_forward_rewrites_destination_port() {
        // RED: Verify port rewriting
        //
        // Given:
        // - Packet with dst port 80 (VIP port)
        // - Backend port 8080
        //
        // Expected:
        // - Packet dst port rewritten to 8080
        //
        // Verification:
        // - Parse TCP header after forward_to_backend()
        // - Check TCP dst port field

        panic!("Test not yet implemented");
    }

    #[test]
    #[ignore]
    fn test_forward_updates_checksums() {
        // RED: Verify checksum updates
        //
        // After rewriting IP/port, checksums must be recalculated:
        // - IP checksum (covers IP header)
        // - TCP checksum (covers TCP header + pseudo-header)
        //
        // Invalid checksums will cause packet drops at receiver.

        panic!("Test not yet implemented");
    }

    #[test]
    #[ignore]
    fn test_forward_handles_fragmented_packets() {
        // RED: Handle IP fragmentation
        //
        // XDP sees raw packets before defragmentation.
        // Strategy: Pass fragmented packets to kernel (XDP_PASS)
        //
        // Expected:
        // - Check IP flags for MF (More Fragments) or frag offset > 0
        // - Return XDP_PASS for fragmented packets

        panic!("Test not yet implemented");
    }
}
