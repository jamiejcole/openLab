// SPDX-License-Identifier: GPL-3.0-or-later

//! What can be said about a Network cue without touching the network (CT-30, NFR-07): whether a
//! destination's host and port can be addressed at all, and whether the OSC message the cue
//! would send is well formed and small enough to send. Pure: whether a host name resolves is
//! never checked before GO (QLab 5 does not flag an unreachable destination).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::cue::OscArg;

/// Longest OSC address pattern sent, in bytes.
pub const MAX_OSC_ADDRESS_BYTES: usize = 512;
/// Most arguments one message carries.
pub const MAX_OSC_ARGS: usize = 32;
/// Longest string argument, in bytes.
pub const MAX_OSC_STRING_BYTES: usize = 4096;
/// Largest encoded message, in bytes. Under macOS's default UDP datagram limit (9216), so a
/// message that passes can be sent as one datagram on every platform.
pub const MAX_OSC_MESSAGE_BYTES: usize = 8192;
/// Longest host name (DNS limit).
pub const MAX_HOST_BYTES: usize = 253;

/// Why a Network cue's message cannot be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageIssue {
    /// No address, or it does not start with `/`.
    NotAnAddress,
    /// Longer than [`MAX_OSC_ADDRESS_BYTES`].
    AddressTooLong,
    /// A space, control character or non-ASCII character in the address.
    AddressCharacter,
    /// More than [`MAX_OSC_ARGS`] arguments.
    TooManyArgs,
    /// A string argument longer than [`MAX_OSC_STRING_BYTES`].
    StringTooLong,
    /// A string argument with a NUL byte (an OSC string ends at the first one).
    StringNul,
    /// A float argument that is not finite (OSC can carry one, but no receiver means it).
    FloatNotFinite,
    /// The encoded message is over [`MAX_OSC_MESSAGE_BYTES`].
    TooLarge,
}

impl fmt::Display for MessageIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotAnAddress => "the OSC address must start with /",
            Self::AddressTooLong => "the OSC address is too long",
            Self::AddressCharacter => {
                "the OSC address has a space, control character or non-ASCII character"
            }
            Self::TooManyArgs => "the message has too many arguments",
            Self::StringTooLong => "a string argument is too long",
            Self::StringNul => "a string argument contains a NUL character",
            Self::FloatNotFinite => "a float argument is not a finite number",
            Self::TooLarge => "the message is too large to send",
        })
    }
}

/// Checks the OSC message a Network cue would send. The address is a pattern as far as OSC is
/// concerned, so `* ? [ ] { }` are fine in it.
pub fn check_message(address: &str, args: &[OscArg]) -> Result<(), MessageIssue> {
    if !address.starts_with('/') {
        return Err(MessageIssue::NotAnAddress);
    }
    if address.len() > MAX_OSC_ADDRESS_BYTES {
        return Err(MessageIssue::AddressTooLong);
    }
    if !address.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(MessageIssue::AddressCharacter);
    }
    if args.len() > MAX_OSC_ARGS {
        return Err(MessageIssue::TooManyArgs);
    }
    // Four bytes of OSC string padding at least, so the sizes below are exact: a string takes
    // its bytes, a NUL, and padding to a multiple of four; the type tag string starts with `,`.
    let padded = |len: usize| (len / 4 + 1) * 4;
    let mut size = padded(address.len()) + padded(1 + args.len());
    for arg in args {
        match arg {
            OscArg::Int { .. } | OscArg::Float { .. } => size += 4,
            OscArg::Bool { .. } => {}
            OscArg::String { value } => {
                if value.len() > MAX_OSC_STRING_BYTES {
                    return Err(MessageIssue::StringTooLong);
                }
                if value.contains('\0') {
                    return Err(MessageIssue::StringNul);
                }
                size += padded(value.len());
            }
        }
        if let OscArg::Float { value } = arg
            && !value.is_finite()
        {
            return Err(MessageIssue::FloatNotFinite);
        }
    }
    if size > MAX_OSC_MESSAGE_BYTES {
        return Err(MessageIssue::TooLarge);
    }
    Ok(())
}

/// Why a network patch's host and port cannot be addressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointIssue {
    EmptyHost,
    /// Longer than [`MAX_HOST_BYTES`].
    HostTooLong,
    /// Neither an IP address nor a valid host name (spaces, a malformed number such as
    /// `300.1.1.1`, a label that is empty or too long, ...).
    BadHost,
    /// Port 0 cannot be sent to.
    ZeroPort,
}

impl fmt::Display for EndpointIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyHost => "the host is empty",
            Self::HostTooLong => "the host name is too long",
            Self::BadHost => "the host is not a valid IP address or host name",
            Self::ZeroPort => "the port must be 1 to 65535",
        })
    }
}

/// What a valid host is, so the binary knows whether it has to look it up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    /// An IPv4 or IPv6 address: nothing to resolve.
    Ip,
    /// A host name: it has to be resolved.
    Name,
}

/// Checks a network patch's address without resolving it. `[::1]` (brackets) is accepted for
/// IPv6, as it is written in URLs.
pub fn check_endpoint(host: &str, port: u16) -> Result<HostKind, EndpointIssue> {
    if host.is_empty() {
        return Err(EndpointIssue::EmptyHost);
    }
    if host.len() > MAX_HOST_BYTES {
        return Err(EndpointIssue::HostTooLong);
    }
    if port == 0 {
        return Err(EndpointIssue::ZeroPort);
    }
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if bare.parse::<Ipv6Addr>().is_ok() || bare.parse::<Ipv4Addr>().is_ok() {
        return Ok(HostKind::Ip);
    }
    // Digits and dots that are not an IPv4 address are a mistyped one, not a name (no top-level
    // domain is numeric), and a resolver would otherwise take its time to say so.
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') || !is_host_name(host) {
        return Err(EndpointIssue::BadHost);
    }
    Ok(HostKind::Name)
}

/// Letters, digits, `-` and `_` in labels of 1 to 63 bytes, a label never starting or ending
/// with `-`; one trailing dot (a fully qualified name) is allowed.
fn is_host_name(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    !host.is_empty()
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> OscArg {
        OscArg::String {
            value: value.into(),
        }
    }

    #[test]
    fn ordinary_messages_pass() {
        assert_eq!(check_message("/lx/go", &[]), Ok(()));
        assert_eq!(
            check_message(
                "/cue/*/{start,stop}",
                &[
                    OscArg::Int { value: -1 },
                    OscArg::Float { value: 0.5 },
                    s("hi"),
                    OscArg::Bool { value: true },
                ]
            ),
            Ok(())
        );
        assert_eq!(check_message("/", &[]), Ok(()));
    }

    #[test]
    fn bad_addresses_are_named() {
        let cases = [
            ("", MessageIssue::NotAnAddress),
            ("go", MessageIssue::NotAnAddress),
            ("/a b", MessageIssue::AddressCharacter),
            ("/a\u{0}b", MessageIssue::AddressCharacter),
            ("/caf\u{e9}", MessageIssue::AddressCharacter),
            ("/a\n", MessageIssue::AddressCharacter),
        ];
        for (address, issue) in cases {
            assert_eq!(check_message(address, &[]), Err(issue), "{address:?}");
        }
        let long = format!("/{}", "a".repeat(MAX_OSC_ADDRESS_BYTES));
        assert_eq!(check_message(&long, &[]), Err(MessageIssue::AddressTooLong));
        let longest = format!("/{}", "a".repeat(MAX_OSC_ADDRESS_BYTES - 1));
        assert_eq!(check_message(&longest, &[]), Ok(()));
    }

    #[test]
    fn argument_limits() {
        let many = vec![OscArg::Int { value: 0 }; MAX_OSC_ARGS + 1];
        assert_eq!(check_message("/a", &many), Err(MessageIssue::TooManyArgs));
        assert_eq!(check_message("/a", &many[1..]), Ok(()));
        assert_eq!(
            check_message("/a", &[s(&"x".repeat(MAX_OSC_STRING_BYTES + 1))]),
            Err(MessageIssue::StringTooLong)
        );
        assert_eq!(
            check_message("/a", &[s("a\0b")]),
            Err(MessageIssue::StringNul)
        );
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                check_message("/a", &[OscArg::Float { value }]),
                Err(MessageIssue::FloatNotFinite)
            );
        }
    }

    #[test]
    fn total_size_is_capped_exactly() {
        // Three strings of 4000 bytes: 3 * 4004 + 8 is over the cap.
        let big = s(&"x".repeat(4000));
        assert_eq!(
            check_message("/a", &[big.clone(), big.clone(), big]),
            Err(MessageIssue::TooLarge)
        );
        // The size is the encoded length: two 4000-byte strings are 8008 + 4 + 4 = 8016.
        let big = s(&"x".repeat(4000));
        assert_eq!(check_message("/a", &[big.clone(), big]), Ok(()));
    }

    #[test]
    fn endpoints() {
        assert_eq!(check_endpoint("192.168.1.20", 53000), Ok(HostKind::Ip));
        assert_eq!(check_endpoint("::1", 9000), Ok(HostKind::Ip));
        assert_eq!(check_endpoint("[fe80::1]", 9000), Ok(HostKind::Ip));
        assert_eq!(check_endpoint("lights.local", 9000), Ok(HostKind::Name));
        assert_eq!(check_endpoint("lx-desk_1.", 9000), Ok(HostKind::Name));
        assert_eq!(check_endpoint("localhost", 9000), Ok(HostKind::Name));
        let cases = [
            ("", 9000, EndpointIssue::EmptyHost),
            ("a", 0, EndpointIssue::ZeroPort),
            ("300.1.1.1", 9000, EndpointIssue::BadHost),
            ("1.2.3", 9000, EndpointIssue::BadHost),
            ("my host", 9000, EndpointIssue::BadHost),
            ("-a.b", 9000, EndpointIssue::BadHost),
            ("a..b", 9000, EndpointIssue::BadHost),
            (".", 9000, EndpointIssue::BadHost),
            ("[::1", 9000, EndpointIssue::BadHost),
            ("http://a", 9000, EndpointIssue::BadHost),
            ("caf\u{e9}.local", 9000, EndpointIssue::BadHost),
        ];
        for (host, port, issue) in cases {
            assert_eq!(check_endpoint(host, port), Err(issue), "{host:?}");
        }
        let label = "a".repeat(64);
        assert_eq!(check_endpoint(&label, 1), Err(EndpointIssue::BadHost));
        let long = format!("{}.com", "a".repeat(MAX_HOST_BYTES));
        assert_eq!(check_endpoint(&long, 1), Err(EndpointIssue::HostTooLong));
    }
}
