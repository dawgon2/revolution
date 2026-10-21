//! Finding a receiver by its channel number (ADR 0008 §4): DNS-SD over mDNS, with a UDP
//! broadcast probe as the fallback when multicast is blocked.
//!
//! The advert holds nothing but the channel (instance name), the TCP port and TXT `v=1`.
//! Probe: `REVNEAR1?<n>` (n = 0 asks every receiver); answer: `REVNEAR1!<n> <tcp port>`.

use super::MAX_CHANNEL;
use super::net::Shared;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The DNS-SD service type receivers advertise.
pub const SERVICE_TYPE: &str = "_revolution-recv._tcp.local.";
/// The UDP port receivers answer broadcast probes on.
pub const PROBE_PORT: u16 = 47820;
/// The most receivers one search returns.
pub const MAX_CANDIDATES: usize = 16;

const PROBE_PREFIX: &str = "REVNEAR1?";
const REPLY_PREFIX: &str = "REVNEAR1!";
/// Broadcast when mDNS has found nothing for this long.
const BROADCAST_AFTER: Duration = Duration::from_secs(2);
/// Once something is found, wait this much longer for others.
const GRACE: Duration = Duration::from_millis(500);

/// A receiver found on the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Its code's channel number.
    pub channel: u8,
    pub addr: SocketAddr,
}

/// A running mDNS advertisement; dropping it withdraws it.
pub(crate) struct Advert {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advert {
    pub fn start(channel: u8, port: u16) -> Result<Advert, String> {
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        let mut tag = [0u8; 4];
        getrandom::getrandom(&mut tag).map_err(|e| e.to_string())?;
        let host = format!("revolution-{}.local.", super::wire::hex(&tag));
        let info = ServiceInfo::new(SERVICE_TYPE, &channel.to_string(), &host, "", port, &[("v", "1")][..]).map_err(|e| e.to_string())?.enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        if let Err(e) = daemon.register(info) {
            let _ = daemon.shutdown();
            return Err(e.to_string());
        }
        Ok(Advert { daemon, fullname })
    }
}

impl Drop for Advert {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// The channel a probe asks for (0 = any), if `bytes` is one.
fn parse_probe(bytes: &[u8]) -> Option<u8> {
    let text = std::str::from_utf8(bytes).ok()?;
    let n: u8 = text.strip_prefix(PROBE_PREFIX)?.trim().parse().ok()?;
    (n <= MAX_CHANNEL).then_some(n)
}

/// `(channel, tcp port)` from a probe answer.
fn parse_reply(bytes: &[u8]) -> Option<(u8, u16)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (n, port) = text.strip_prefix(REPLY_PREFIX)?.trim().split_once(' ')?;
    let n: u8 = n.parse().ok()?;
    let port: u16 = port.trim().parse().ok()?;
    ((1..=MAX_CHANNEL).contains(&n) && port != 0).then_some((n, port))
}

/// Receiver side: answers probes for `channel` until stopped.
pub(crate) fn answer_probes(sock: &UdpSocket, channel: u8, tcp_port: u16, shared: &Shared) {
    if sock.set_read_timeout(Some(Duration::from_millis(200))).is_err() {
        return;
    }
    let reply = format!("{REPLY_PREFIX}{channel} {tcp_port}");
    let mut buf = [0u8; 64];
    while !shared.stop.load(Ordering::Relaxed) {
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            let asked = buf.get(..n).and_then(parse_probe);
            if shared.usable() && (asked == Some(channel) || asked == Some(0)) {
                let _ = sock.send_to(reply.as_bytes(), from);
            }
        }
    }
}

/// Collects candidates, deduplicated and capped.
struct Found {
    want: Option<u8>,
    list: Vec<Candidate>,
    first_at: Option<Instant>,
}

impl Found {
    fn add(&mut self, c: Candidate) {
        if self.want.is_some_and(|w| w != c.channel) || self.list.len() >= MAX_CANDIDATES || self.list.contains(&c) {
            return;
        }
        self.list.push(c);
        self.first_at.get_or_insert_with(Instant::now);
    }
    fn settled(&self) -> bool {
        self.list.len() >= MAX_CANDIDATES || self.first_at.is_some_and(|t| t.elapsed() >= GRACE)
    }
}

fn instance_channel(fullname: &str) -> Option<u8> {
    let instance = fullname.strip_suffix(SERVICE_TYPE)?.strip_suffix('.')?;
    let digits: String = instance.chars().take_while(char::is_ascii_digit).take(3).collect();
    let n: u8 = digits.parse().ok()?;
    (1..=MAX_CHANNEL).contains(&n).then_some(n)
}

fn usable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_unspecified() && !v4.is_broadcast() && !v4.is_multicast(),
        // Link-local IPv6 needs a scope id we don't carry; the IPv4 address is used instead.
        IpAddr::V6(v6) => !v6.is_unspecified() && !v6.is_multicast() && (v6.segments().first().copied().unwrap_or(0) & 0xffc0) != 0xfe80,
    }
}

fn probe_socket() -> Option<UdpSocket> {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.set_broadcast(true).ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(50))).ok()?;
    Some(sock)
}

fn read_replies(sock: &UdpSocket, found: &mut Found) {
    let mut buf = [0u8; 64];
    // Bounded: at most a few dozen packets per call.
    for _ in 0..32 {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Some((channel, port)) = buf.get(..n).and_then(parse_reply) {
                    found.add(Candidate { channel, addr: SocketAddr::new(from.ip(), port) });
                }
            }
            Err(_) => return,
        }
    }
}

fn search(want: Option<u8>, timeout: Duration, probe_port: u16) -> Vec<Candidate> {
    let start = Instant::now();
    let until = start.checked_add(timeout).unwrap_or(start);
    let mut found = Found { want, list: Vec::new(), first_at: None };
    let daemon = ServiceDaemon::new().ok();
    let events = daemon.as_ref().and_then(|d| d.browse(SERVICE_TYPE).ok());
    let mut udp: Option<UdpSocket> = None;
    let mut probed = false;
    while Instant::now() < until && !found.settled() {
        if let Some(rx) = &events {
            let wait = Duration::from_millis(50).min(until.saturating_duration_since(Instant::now()));
            if let Ok(ServiceEvent::ServiceResolved(info)) = rx.recv_timeout(wait) {
                let txt_ok = info.get_property_val_str("v") == Some("1");
                if let (Some(channel), true) = (instance_channel(info.get_fullname()), txt_ok) {
                    let mut ips: Vec<IpAddr> = info.get_addresses().iter().map(|a| a.to_ip_addr()).filter(|ip| usable_ip(*ip)).collect();
                    ips.sort_by_key(|ip| ip.is_ipv6());
                    for ip in ips.into_iter().take(MAX_CANDIDATES) {
                        found.add(Candidate { channel, addr: SocketAddr::new(ip, info.get_port()) });
                    }
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
        let quiet = found.list.is_empty() && (start.elapsed() >= BROADCAST_AFTER || events.is_none());
        if quiet && !probed {
            probed = true;
            udp = probe_socket();
            if let Some(sock) = &udp {
                let msg = format!("{PROBE_PREFIX}{}", want.unwrap_or(0));
                let _ = sock.send_to(msg.as_bytes(), (Ipv4Addr::BROADCAST, probe_port));
            }
        }
        if let Some(sock) = &udp {
            read_replies(sock, &mut found);
        }
    }
    if let Some(d) = daemon {
        let _ = d.stop_browse(SERVICE_TYPE);
        let _ = d.shutdown();
    }
    found.list
}

/// Receivers waiting on `channel` (at most [`MAX_CANDIDATES`]), searching for up to `timeout`.
pub fn discover(channel: u8, timeout: Duration) -> Vec<Candidate> {
    search(Some(channel), timeout, PROBE_PORT)
}

/// Every receiver currently waiting (channel + address), at most [`MAX_CANDIDATES`].
pub fn list_receivers(timeout: Duration) -> Vec<Candidate> {
    search(None, timeout, PROBE_PORT)
}

/// Asks specific hosts (a typed address without a port) for their TCP port.
pub(crate) fn probe_hosts(ips: &[IpAddr], channel: u8, probe_port: u16, timeout: Duration) -> Vec<Candidate> {
    let mut found = Found { want: Some(channel), list: Vec::new(), first_at: None };
    let bind = if ips.iter().any(IpAddr::is_ipv6) { "[::]:0" } else { "0.0.0.0:0" };
    let Ok(sock) = UdpSocket::bind(bind) else { return Vec::new() };
    if sock.set_read_timeout(Some(Duration::from_millis(50))).is_err() {
        return Vec::new();
    }
    let msg = format!("{PROBE_PREFIX}{channel}");
    for ip in ips.iter().take(MAX_CANDIDATES) {
        let _ = sock.send_to(msg.as_bytes(), SocketAddr::new(*ip, probe_port));
    }
    let until = super::after(timeout);
    while Instant::now() < until && !found.settled() {
        read_replies(&sock, &mut found);
    }
    found.list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_and_reply_parsing() {
        assert_eq!(parse_probe(b"REVNEAR1?7"), Some(7));
        assert_eq!(parse_probe(b"REVNEAR1?0"), Some(0));
        assert_eq!(parse_probe(b"REVNEAR1?100"), None);
        assert_eq!(parse_probe(b"REVNEAR1!7"), None);
        assert_eq!(parse_probe(&[0xff, 0xfe]), None);
        assert_eq!(parse_reply(b"REVNEAR1!7 5000"), Some((7, 5000)));
        assert_eq!(parse_reply(b"REVNEAR1!7 0"), None);
        assert_eq!(parse_reply(b"REVNEAR1!0 5000"), None);
        assert_eq!(parse_reply(b"REVNEAR1!7 99999"), None);
        assert_eq!(parse_reply(b"REVNEAR1!7"), None);
    }

    #[test]
    fn instance_names() {
        assert_eq!(instance_channel("7._revolution-recv._tcp.local."), Some(7));
        assert_eq!(instance_channel("42 (2)._revolution-recv._tcp.local."), Some(42));
        assert_eq!(instance_channel("0._revolution-recv._tcp.local."), None);
        assert_eq!(instance_channel("x._revolution-recv._tcp.local."), None);
        assert_eq!(instance_channel("7._http._tcp.local."), None);
    }

    #[test]
    fn found_caps_and_filters() {
        let mut f = Found { want: Some(3), list: Vec::new(), first_at: None };
        for port in 1..100u16 {
            f.add(Candidate { channel: 3, addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port) });
            f.add(Candidate { channel: 4, addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port) });
        }
        assert_eq!(f.list.len(), MAX_CANDIDATES);
        assert!(f.list.iter().all(|c| c.channel == 3));
        assert!(f.settled());
    }
}
