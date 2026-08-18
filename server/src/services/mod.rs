pub mod fan_out;
pub mod heartbeat;
pub mod video_scan;

use std::net::IpAddr;

/// Port every Pi agent listens on.
///
/// The agent's own port is configurable via `AGENT_PORT`, but `devices` stores
/// no port column, so the server assumes the default. An agent moved off 8080
/// becomes unreachable — that would need a port column and a `RegisterRequest`
/// field to fix properly.
pub const AGENT_PORT: u16 = 8080;

/// `http://host:8080`, bracketing the host if it is an IPv6 literal.
pub fn agent_base_url(ip: &str) -> String {
    match ip.parse::<IpAddr>() {
        Ok(IpAddr::V6(addr)) => format!("http://[{addr}]:{AGENT_PORT}"),
        // Hostnames and IPv4 both work unbracketed.
        _ => format!("http://{ip}:{AGENT_PORT}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_url_brackets_ipv6_literals() {
        assert_eq!(agent_base_url("192.168.1.11"), "http://192.168.1.11:8080");
        assert_eq!(agent_base_url("tv-01.local"), "http://tv-01.local:8080");
        assert_eq!(agent_base_url("fe80::1"), "http://[fe80::1]:8080");
    }
}
