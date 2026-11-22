// SPDX-License-Identifier: GPL-3.0-or-later

//! Sockets bound to one network interface, for a network patch that names one (QLab 5's
//! per-patch interface choice). Binding is by interface, not by address, so the choice holds
//! when the interface's address changes. Where the system cannot do it the send fails with
//! that said, and the show goes on.

use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

/// A UDP socket for `interface`, bound to any port.
pub(super) fn udp_socket(v4: bool, interface: &str) -> io::Result<UdpSocket> {
    let (domain, any) = if v4 {
        (Domain::IPV4, SocketAddr::from(([0, 0, 0, 0], 0)))
    } else {
        (Domain::IPV6, SocketAddr::from(([0u16; 8], 0)))
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    bind_device(&socket, interface, v4)?;
    socket.bind(&SockAddr::from(any))?;
    Ok(socket.into())
}

/// A TCP connection to `to` leaving by `interface`.
pub(super) fn tcp_connect(
    to: &SocketAddr,
    interface: &str,
    timeout: Duration,
) -> io::Result<TcpStream> {
    let socket = Socket::new(Domain::for_address(*to), Type::STREAM, Some(Protocol::TCP))?;
    bind_device(&socket, interface, to.is_ipv4())?;
    socket.connect_timeout(&SockAddr::from(*to), timeout)?;
    Ok(socket.into())
}

#[cfg(target_os = "macos")]
fn bind_device(socket: &Socket, interface: &str, v4: bool) -> io::Result<()> {
    let index = index_of(interface)?;
    if v4 {
        socket.bind_device_by_index_v4(Some(index))
    } else {
        socket.bind_device_by_index_v6(Some(index))
    }
}

/// The system's number for the interface named `name`.
#[cfg(target_os = "macos")]
fn index_of(name: &str) -> io::Result<std::num::NonZeroU32> {
    use std::ffi::{CString, c_char, c_uint};
    unsafe extern "C" {
        fn if_nametoindex(name: *const c_char) -> c_uint;
    }
    let c_name = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad interface name"))?;
    // SAFETY: `c_name` is a NUL-terminated string that outlives the call, which only reads it.
    let index = unsafe { if_nametoindex(c_name.as_ptr()) };
    std::num::NonZeroU32::new(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no network interface named {name}"),
        )
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn bind_device(socket: &Socket, interface: &str, _v4: bool) -> io::Result<()> {
    socket.bind_device(Some(interface.as_bytes()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn bind_device(_: &Socket, _: &str, _: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "choosing a network interface is not supported on this system",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    use std::io::{Read, Write};
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    use std::net::{Ipv4Addr, TcpListener};

    /// The loopback interface's name on this system.
    #[cfg(target_os = "macos")]
    const LOOPBACK: &str = "lo0";
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const LOOPBACK: &str = "lo";

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    #[test]
    fn a_udp_socket_bound_to_the_loopback_interface_reaches_a_listener_on_it() {
        let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let sender = udp_socket(true, LOOPBACK).unwrap();
        sender
            .send_to(b"ping", listener.local_addr().unwrap())
            .unwrap();
        let mut buf = [0u8; 8];
        let (n, _) = listener.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    #[test]
    fn a_tcp_connection_bound_to_the_loopback_interface_reaches_a_listener_on_it() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut out = tcp_connect(
            &listener.local_addr().unwrap(),
            LOOPBACK,
            Duration::from_secs(5),
        )
        .unwrap();
        out.write_all(b"ping").unwrap();
        drop(out);
        let (mut got, _) = listener.accept().unwrap();
        let mut text = Vec::new();
        got.read_to_end(&mut text).unwrap();
        assert_eq!(text, b"ping");
    }

    /// An interface the computer does not have fails the send with its name, on every path.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_interface_the_computer_lacks_is_refused_by_name() {
        let e = udp_socket(true, "nosuch9").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("nosuch9"), "{e}");
        let to = SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
        let e = tcp_connect(&to, "nosuch9", Duration::from_secs(1)).unwrap_err();
        assert!(e.to_string().contains("nosuch9"), "{e}");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn an_interface_the_computer_lacks_is_refused() {
        assert!(udp_socket(true, "nosuch9").is_err());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    #[test]
    fn choosing_an_interface_says_it_is_not_supported_here() {
        let e = udp_socket(true, "Ethernet").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Unsupported);
    }
}
