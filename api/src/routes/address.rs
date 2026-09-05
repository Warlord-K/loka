use std::net::SocketAddr;

use axum::http::HeaderMap;

pub struct ClientAddress {
    pub ip: Option<String>,
    pub certain: bool,
}

fn address_header() -> Option<String> {
    std::env::var("WORLD_CLIENT_IP_HEADER").ok().map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty())
}

fn last_outside_hop(forwarded: &str) -> Option<&str> {
    let hops: Vec<&str> = forwarded.split(',').map(str::trim).filter(|h| !h.is_empty()).collect();
    hops.iter().rev().find(|hop| !hop.parse::<std::net::IpAddr>().is_ok_and(trusted_proxy)).or(hops.first()).copied()
}

pub fn client_address(headers: &HeaderMap, peer: Option<SocketAddr>) -> ClientAddress {
    resolve_address(headers, peer, address_header().as_deref())
}

fn resolve_address(headers: &HeaderMap, peer: Option<SocketAddr>, named: Option<&str>) -> ClientAddress {
    let peer_ip = peer.map(|p| p.ip().to_string());
    if peer.is_some_and(|peer| !trusted_proxy(peer.ip())) {
        return ClientAddress { ip: peer_ip, certain: true };
    }
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    let forwarded = || header("x-forwarded-for").and_then(last_outside_hop);
    match named {
        Some("peer") => ClientAddress { ip: peer_ip, certain: peer.is_some() },
        Some("x-forwarded-for") => ClientAddress { ip: forwarded().map(String::from).or(peer_ip), certain: true },
        Some(name) => ClientAddress { ip: header(name).map(String::from).or(peer_ip), certain: true },
        None => ClientAddress {
            ip: header("cf-connecting-ip").or_else(|| header("x-real-ip")).or_else(forwarded).map(String::from).or(peer_ip),
            certain: false,
        },
    }
}

fn trusted_proxy(ip: std::net::IpAddr) -> bool {
    let private = match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_private() || v4.is_loopback()),
    };
    if private {
        return true;
    }
    std::env::var("WORLD_TRUSTED_PROXIES").ok().is_some_and(|list| {
        list.split(',').map(str::trim).filter(|entry| !entry.is_empty()).any(|entry| match entry.split_once('/') {
            Some((net, bits)) => in_network(ip, net, bits),
            None => entry.parse::<std::net::IpAddr>().is_ok_and(|trusted| trusted == ip),
        })
    })
}

fn in_network(ip: std::net::IpAddr, net: &str, bits: &str) -> bool {
    use std::net::IpAddr;
    let Ok(bits) = bits.parse::<u32>() else { return false };
    match (ip, net.parse::<IpAddr>()) {
        (IpAddr::V4(ip), Ok(IpAddr::V4(net))) if bits <= 32 => {
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            u32::from(ip) & mask == u32::from(net) & mask
        }
        (IpAddr::V6(ip), Ok(IpAddr::V6(net))) if bits <= 128 => {
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            u128::from(ip) & mask == u128::from(net) & mask
        }
        _ => false,
    }
}
