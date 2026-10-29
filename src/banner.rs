//! Startup banner: LAN URLs and a QR code for the first one.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;

/// Interface-name fragments (lowercase) that mark virtual adapters the Quest cannot reach.
const VIRTUAL_MARKERS: [&str; 6] = [
    "vethernet",
    "virtualbox",
    "vmware",
    "wsl",
    "hyper-v",
    "docker",
];

fn is_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("loopback") || VIRTUAL_MARKERS.iter().any(|m| n.contains(m))
}

/// Sort rank: 192.168/16 first, then 10/8, then 172.16/12.
fn rank(ip: Ipv4Addr) -> u8 {
    match ip.octets() {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        _ => 2,
    }
}

/// Private LAN IPv4 only: RFC1918, excluding loopback and link-local.
fn is_lan_v4(ip: Ipv4Addr) -> bool {
    crate::net::is_allowed(IpAddr::V4(ip)) && !ip.is_loopback() && !ip.is_link_local()
}

/// Choose the addresses to advertise, most likely home LAN first.
///
/// A specific `--bind` address is shown alone. Otherwise the private IPv4 addresses of
/// `ifaces` are used, skipping virtual adapters unless that would leave nothing.
pub fn select_addresses(ifaces: &[(String, IpAddr)], bind: IpAddr) -> Vec<IpAddr> {
    if !bind.is_unspecified() {
        return vec![bind];
    }
    let lan: Vec<(&str, Ipv4Addr)> = ifaces
        .iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(v4) if is_lan_v4(*v4) => Some((name.as_str(), *v4)),
            _ => None,
        })
        .collect();
    let mut chosen: Vec<Ipv4Addr> = lan
        .iter()
        .filter(|(name, _)| !is_virtual(name))
        .map(|(_, ip)| *ip)
        .collect();
    if chosen.is_empty() {
        chosen = lan.iter().map(|(_, ip)| *ip).collect();
    }
    chosen.sort_by_key(|ip| (rank(*ip), *ip));
    chosen.dedup();
    chosen.into_iter().map(IpAddr::V4).collect()
}

fn url(ip: IpAddr, port: u16) -> String {
    format!("http://{}", SocketAddr::new(ip, port))
}

/// Render `text` as a QR code with a quiet zone, for dark terminals: filled half-blocks
/// are drawn as light, so light modules (and the quiet zone) use the filled glyph.
pub fn render_qr(text: &str) -> Option<String> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    Some(
        code.render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

/// Build the full banner text.
pub fn render(
    version: &str,
    videos: usize,
    libraries: usize,
    addrs: &[IpAddr],
    port: u16,
    qr: bool,
) -> String {
    let mut out = format!(
        "matinee v{version} \u{2014} {videos} video{} in {libraries} librar{}\n",
        if videos == 1 { "" } else { "s" },
        if libraries == 1 { "y" } else { "ies" },
    );
    let first = if let Some(first) = addrs.first() {
        out.push_str("Open in DeoVR (built-in browser) or any browser on your network:\n");
        for ip in addrs {
            out.push_str(&format!("  {}\n", url(*ip, port)));
        }
        url(*first, port)
    } else {
        let local = format!("http://localhost:{port}");
        out.push_str(&format!("  {local}\n"));
        out.push_str(
            "No local network address found; the Quest won't be able to reach this. \
             Connect this PC to your home network.\n",
        );
        return out + "Press Ctrl+C to stop.\n";
    };
    if qr {
        if let Some(art) = render_qr(&first) {
            out.push_str(&art);
            out.push('\n');
        }
    }
    out.push_str("Press Ctrl+C to stop.\n");
    out
}

/// Print the banner to stdout.
pub fn print(videos: usize, libraries: usize, bind: IpAddr, port: u16, qr: bool) {
    let ifaces: Vec<(String, IpAddr)> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let addrs = select_addresses(&ifaces, bind);
    print!(
        "{}",
        render(
            env!("CARGO_PKG_VERSION"),
            videos,
            libraries,
            &addrs,
            port,
            qr
        )
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nic(name: &str, ip: &str) -> (String, IpAddr) {
        (name.to_string(), ip.parse().unwrap())
    }
    fn any() -> IpAddr {
        "0.0.0.0".parse().unwrap()
    }
    fn s(v: Vec<IpAddr>) -> Vec<String> {
        v.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn orders_192_then_10_then_172_and_dedupes() {
        let n = vec![
            nic("a", "172.20.0.2"),
            nic("b", "10.0.0.7"),
            nic("c", "192.168.1.50"),
            nic("d", "192.168.1.50"),
            nic("e", "127.0.0.1"),
            nic("f", "169.254.1.1"),
            nic("g", "8.8.8.8"),
            nic("h", "fe80::1"),
        ];
        assert_eq!(
            s(select_addresses(&n, any())),
            ["192.168.1.50", "10.0.0.7", "172.20.0.2"]
        );
    }

    #[test]
    fn filters_virtual_adapters() {
        let n = vec![
            nic("vEthernet (WSL)", "172.28.0.1"),
            nic("VirtualBox Host-Only", "192.168.56.1"),
            nic("Ethernet", "10.0.0.7"),
        ];
        assert_eq!(s(select_addresses(&n, any())), ["10.0.0.7"]);
    }

    #[test]
    fn falls_back_to_virtual_when_only_those() {
        let n = vec![nic("vEthernet (Default Switch)", "172.28.0.1")];
        assert_eq!(s(select_addresses(&n, any())), ["172.28.0.1"]);
    }

    #[test]
    fn explicit_bind_shows_only_that() {
        let n = vec![nic("Ethernet", "10.0.0.7")];
        let bind: IpAddr = "192.168.1.9".parse().unwrap();
        assert_eq!(s(select_addresses(&n, bind)), ["192.168.1.9"]);
    }

    #[test]
    fn none_found() {
        let n = vec![nic("lo", "127.0.0.1")];
        assert!(select_addresses(&n, any()).is_empty());
        let text = render("0.1.0", 1, 1, &[], 8080, true);
        assert!(text.contains("http://localhost:8080"));
        assert!(text.contains("Quest won't be able"));
        assert!(text.contains("1 video in 1 library"));
    }

    #[test]
    fn banner_has_urls_and_qr() {
        let addrs: Vec<IpAddr> = vec!["192.168.1.50".parse().unwrap()];
        let with = render("0.1.0", 42, 2, &addrs, 8080, true);
        assert!(with.contains("42 videos in 2 libraries"));
        assert!(with.contains("  http://192.168.1.50:8080\n"));
        assert!(with.contains('\u{2588}') || with.contains('\u{2580}'));
        let without = render("0.1.0", 42, 2, &addrs, 8080, false);
        assert!(!without.contains('\u{2588}'));
        assert!(without.ends_with("Press Ctrl+C to stop.\n"));
    }
}
