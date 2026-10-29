//! LAN-only access control.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Whether a peer address is loopback, private (RFC1918), link-local, or IPv6 ULA.
pub fn is_allowed(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_allowed(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Middleware rejecting non-LAN peers with 403. Fails closed when the peer is unknown.
pub async fn lan_only(req: Request, next: Next) -> Response {
    let allowed = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ci| is_allowed(ci.0.ip()));
    if allowed {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "forbidden: LAN access only").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        is_allowed(s.parse().unwrap())
    }

    #[test]
    fn allows_local_ranges() {
        for s in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.50",
            "169.254.10.10",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "::ffff:192.168.1.5",
            "::ffff:127.0.0.1",
        ] {
            assert!(ok(s), "{s} should be allowed");
        }
    }

    #[test]
    fn denies_public_ranges() {
        for s in [
            "8.8.8.8",
            "172.32.0.1",
            "172.15.0.1",
            "192.169.0.1",
            "11.0.0.1",
            "100.64.0.1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
            "fec0::1",
        ] {
            assert!(!ok(s), "{s} should be denied");
        }
    }
}
