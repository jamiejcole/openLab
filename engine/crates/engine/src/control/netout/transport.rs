// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

/// Where a message goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub host: String,
    pub port: u16,
    pub protocol: NetworkProtocol,
    /// The network interface to send from; none: the system routes it.
    pub interface: Option<String>,
}

impl Destination {
    pub(super) fn of(patch: &NetworkPatch) -> Self {
        Self {
            host: patch.host.clone(),
            port: patch.port,
            protocol: patch.protocol,
            interface: patch.interface.clone(),
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let proto = match self.protocol {
            NetworkProtocol::Udp => "udp",
            NetworkProtocol::Tcp => "tcp",
        };
        write!(f, "{proto}://{}:{}", self.host, self.port)?;
        match &self.interface {
            Some(interface) => write!(f, " via {interface}"),
            None => Ok(()),
        }
    }
}

/// The things done on the network, replaceable in tests. All block, and only ever run on
/// worker threads.
pub trait Transport: Send + Sync {
    /// Delivers one packet (UDP: a datagram; TCP: connects and writes the SLIP frame).
    /// `resolved` is what the last successful look-up of the host found, empty if there is
    /// none (an IP address, or a name not looked up yet): then the sender resolves it itself.
    fn send(&self, to: &Destination, resolved: &[SocketAddr], packet: &[u8]) -> Result<(), String>;
    /// The addresses of `host`: a name look-up only.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
    /// Milliseconds on a monotonic clock; only differences matter. The age of a queued
    /// message is measured on it (tests move it by hand).
    fn now_ms(&self) -> u64 {
        monotonic_ms()
    }
}

pub(super) fn set_cache(cache: &Resolved, addrs: &[SocketAddr]) {
    let mut c = crate::lock(cache);
    c.clear();
    c.extend_from_slice(addrs);
}

pub(super) fn monotonic_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    let base = BASE.get_or_init(Instant::now);
    u64::try_from(base.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The real network.
#[derive(Default)]
pub struct SystemTransport {
    /// By address family (true: IPv4) and interface (none: the system's choice).
    udp: Mutex<HashMap<(bool, Option<String>), Arc<UdpSocket>>>,
}

/// Sockets kept at once. A show names a handful of interfaces; past this the table starts
/// again (a sender holding a dropped socket keeps using it).
const MAX_UDP_SOCKETS: usize = 16;

impl SystemTransport {
    /// One socket per address family and interface, kept so every UDP message leaves from the
    /// same source port (a receiver that answers, or filters by source, sees one sender).
    fn udp(&self, to: &SocketAddr, interface: Option<&str>) -> Result<Arc<UdpSocket>, String> {
        let v4 = to.is_ipv4();
        let key = (v4, interface.map(str::to_owned));
        let mut sockets = self
            .udp
            .lock()
            .map_err(|_| "socket state poisoned".to_owned())?;
        if let Some(socket) = sockets.get(&key) {
            return Ok(Arc::clone(socket));
        }
        let socket = match interface {
            Some(name) => interface::udp_socket(v4, name).map_err(|e| e.to_string())?,
            None => {
                let bind = if v4 {
                    SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
                } else {
                    SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
                };
                UdpSocket::bind(bind).map_err(|e| e.to_string())?
            }
        };
        let socket = Arc::new(socket);
        if sockets.len() >= MAX_UDP_SOCKETS {
            sockets.clear();
        }
        sockets.insert(key, Arc::clone(&socket));
        Ok(socket)
    }
}

pub(super) fn addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let found: Vec<SocketAddr> = (bare, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    if found.is_empty() {
        Err("the host has no address".to_owned())
    } else {
        Ok(found)
    }
}

/// IPv4 addresses first, each family in the resolver's order. Most OSC receivers bind IPv4
/// only, and on macOS a name such as `localhost` can resolve to `::1` first, which a UDP
/// sender would never learn was the wrong choice (QLab's behaviour here is unverified).
pub fn prefer_ipv4(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let (v4, v6): (Vec<SocketAddr>, Vec<SocketAddr>) =
        addrs.iter().copied().partition(SocketAddr::is_ipv4);
    v4.into_iter().chain(v6).collect()
}

impl Transport for SystemTransport {
    fn send(&self, to: &Destination, resolved: &[SocketAddr], packet: &[u8]) -> Result<(), String> {
        let found = if resolved.is_empty() {
            addresses(&to.host, to.port)?
        } else {
            resolved.to_vec()
        };
        let found = prefer_ipv4(&found);
        match to.protocol {
            NetworkProtocol::Udp => {
                let first = found.first().ok_or("the host has no address")?;
                let socket = self.udp(first, to.interface.as_deref())?;
                socket
                    .send_to(packet, first)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            NetworkProtocol::Tcp => {
                let mut last = "the host has no address".to_owned();
                for addr in found {
                    let connected = match to.interface.as_deref() {
                        Some(name) => interface::tcp_connect(&addr, name, TCP_TIMEOUT),
                        None => TcpStream::connect_timeout(&addr, TCP_TIMEOUT),
                    };
                    match connected {
                        Ok(mut stream) => {
                            stream
                                .set_write_timeout(Some(TCP_TIMEOUT))
                                .and_then(|()| stream.set_nodelay(true))
                                .and_then(|()| stream.write_all(packet))
                                .and_then(|()| stream.flush())
                                .map_err(|e| e.to_string())?;
                            return Ok(());
                        }
                        Err(e) => last = e.to_string(),
                    }
                }
                Err(last)
            }
        }
    }

    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        addresses(host, port)
    }
}

/// The transport a [`Control`](super::Control) uses.
#[derive(Clone)]
pub struct NetTransport(pub Arc<dyn Transport>);

impl Default for NetTransport {
    fn default() -> Self {
        Self(Arc::new(SystemTransport::default()))
    }
}

impl fmt::Debug for NetTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NetTransport")
    }
}
