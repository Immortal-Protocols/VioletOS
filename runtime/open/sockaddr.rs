//! Socket addresses as apps write them (struct sockaddr_in, sockaddr_in6),
//! and as the runtime gives them back.
//!
//! The runtime's sockets carry IPv4 addresses only (net.rs, loopback.rs). An
//! AF_INET6 socket works with the IPv6 forms of those addresses: an
//! IPv4-mapped address (::ffff:a.b.c.d) is that IPv4 address, ::1 is
//! 127.0.0.1 and :: is the wildcard. Any other IPv6 address has no route
//! (ENETUNREACH; EADDRNOTAVAIL for bind): Radio's lwIP is built without IPv6.
//! Addresses given back to an AF_INET6 socket take the same forms, 127.0.0.1
//! as ::1. Pure: tested on the host (make test-unit).

pub const AF_UNIX: u32 = 1;
pub const AF_INET: u32 = 2;
pub const AF_INET6: u32 = 10;
pub const LOOPBACK: u32 = 0x7f00_0001;
pub const EINVAL: i32 = -22;
pub const EAFNOSUPPORT: i32 = -97;
pub const ENETUNREACH: i32 = -101;
/// The most an address takes (sockaddr_in6).
pub const MAX: usize = 28;

/// The IPv4 address and port in an app's sockaddr (`b`: the bytes it gave,
/// at most MAX). An AF_INET6 socket (`v6`) takes both families, as Linux's
/// connect and sendto do; an AF_INET socket only AF_INET.
pub fn parse(b: &[u8], v6: bool) -> Result<(u32, u16), i32> {
    if b.len() < 8 {
        return Err(EINVAL);
    }
    let port = u16::from_be_bytes([b[2], b[3]]);
    match u16::from_le_bytes([b[0], b[1]]) as u32 {
        AF_INET => Ok((u32::from_be_bytes([b[4], b[5], b[6], b[7]]), port)),
        AF_INET6 if v6 => {
            if b.len() < 24 {
                return Err(EINVAL);
            }
            let a = &b[8..24];
            let v4 = u32::from_be_bytes([a[12], a[13], a[14], a[15]]);
            if a[..10].iter().all(|&x| x == 0) && a[10] == 0xff && a[11] == 0xff {
                Ok((v4, port))
            } else if a[..12].iter().all(|&x| x == 0) && (v4 == 0 || v4 == 1) {
                Ok((if v4 == 1 { LOOPBACK } else { 0 }, port))
            } else {
                Err(ENETUNREACH)
            }
        }
        _ => Err(EAFNOSUPPORT),
    }
}

/// An address as an app of this family reads it, and its length.
pub fn encode(addr: (u32, u16), v6: bool) -> ([u8; MAX], usize) {
    let mut b = [0u8; MAX];
    b[2..4].copy_from_slice(&addr.1.to_be_bytes());
    if !v6 {
        b[..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
        b[4..8].copy_from_slice(&addr.0.to_be_bytes());
        return (b, 16);
    }
    b[..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
    match addr.0 {
        0 => {}
        LOOPBACK => b[23] = 1,
        ip => {
            b[18] = 0xff;
            b[19] = 0xff;
            b[20..24].copy_from_slice(&ip.to_be_bytes());
        }
    }
    (b, MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v6(addr: [u8; 16], port: u16) -> [u8; 28] {
        let mut b = [0u8; 28];
        b[..2].copy_from_slice(&10u16.to_le_bytes());
        b[2..4].copy_from_slice(&port.to_be_bytes());
        b[8..24].copy_from_slice(&addr);
        b
    }

    #[test]
    fn ipv4_round_trips() {
        let (b, n) = encode((0x0a00_0009, 8080), false);
        assert_eq!(n, 16);
        assert_eq!(&b[..8], &[2, 0, 0x1f, 0x90, 10, 0, 0, 9]);
        assert_eq!(parse(&b[..n], false), Ok((0x0a00_0009, 8080)));
        // A v6 socket may connect to an AF_INET address, as on Linux.
        assert_eq!(parse(&b[..n], true), Ok((0x0a00_0009, 8080)));
    }

    #[test]
    fn mapped_loopback_and_wildcard_are_ipv4() {
        let mut mapped = [0u8; 16];
        mapped[10] = 0xff;
        mapped[11] = 0xff;
        mapped[12..].copy_from_slice(&[198, 18, 0, 7]);
        assert_eq!(parse(&v6(mapped, 443), true), Ok((0xc612_0007, 443)));
        let mut one = [0u8; 16];
        one[15] = 1;
        assert_eq!(parse(&v6(one, 80), true), Ok((LOOPBACK, 80)));
        assert_eq!(parse(&v6([0; 16], 9), true), Ok((0, 9)));
        // sockaddr_in6 without its scope id (RFC 2133's 24 bytes) is accepted.
        assert_eq!(parse(&v6(one, 80)[..24], true), Ok((LOOPBACK, 80)));
    }

    #[test]
    fn real_ipv6_has_no_route() {
        let mut global = [0u8; 16];
        global[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
        global[15] = 1;
        assert_eq!(parse(&v6(global, 443), true), Err(ENETUNREACH));
        // ::0.0.0.2 (IPv4-compatible, deprecated) and ::ffff:0:0:1 lookalikes are not mapped.
        let mut compat = [0u8; 16];
        compat[15] = 2;
        assert_eq!(parse(&v6(compat, 1), true), Err(ENETUNREACH));
        let mut near = [0u8; 16];
        near[9] = 0xff;
        near[10] = 0xff;
        near[11] = 0xff;
        assert_eq!(parse(&v6(near, 1), true), Err(ENETUNREACH));
    }

    #[test]
    fn wrong_family_or_short_is_refused() {
        assert_eq!(parse(&v6([0; 16], 1), false), Err(EAFNOSUPPORT));
        assert_eq!(parse(&[1, 0, 0, 0, 0, 0, 0, 0], true), Err(EAFNOSUPPORT));
        assert_eq!(parse(&[2, 0, 0, 0], false), Err(EINVAL));
        assert_eq!(parse(&v6([0; 16], 1)[..20], true), Err(EINVAL));
    }

    #[test]
    fn v6_sockets_read_v6_forms() {
        let (b, n) = encode((LOOPBACK, 5000), true);
        assert_eq!(n, 28);
        assert_eq!(&b[..4], &[10, 0, 0x13, 0x88]);
        assert_eq!(&b[8..24], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let (b, _) = encode((0xc612_0007, 443), true);
        assert_eq!(&b[8..24], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 198, 18, 0, 7]);
        assert_eq!(parse(&b, true), Ok((0xc612_0007, 443)));
        let (b, _) = encode((0, 7), true);
        assert_eq!(parse(&b, true), Ok((0, 7)));
        assert!(b[8..28].iter().all(|&x| x == 0));
    }
}
