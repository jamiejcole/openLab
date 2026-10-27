// SPDX-License-Identifier: GPL-3.0-or-later

//! OSC bytes <-> messages: datagram decoding with limits, the incremental SLIP decoder for TCP
//! streams, and encoding of replies.
//!
//! `rosc` decodes bundles by recursion with no depth limit, so bundles are walked here (with a
//! depth limit) and `rosc` only ever sees single messages. Nested array type tags are checked
//! before `rosc` builds them: dropping a deeply nested value would recurse too.

use rosc::{OscMessage, OscPacket};

/// Largest datagram or SLIP frame accepted. Commands are tiny; this bounds memory per packet.
pub const MAX_PACKET_BYTES: usize = 64 * 1024;
/// Most bundles inside one another (the outermost counts as 1).
pub const MAX_BUNDLE_DEPTH: usize = 4;
/// Most messages one packet may flatten to.
pub const MAX_MESSAGES_PER_PACKET: usize = 128;
/// Deepest `[` nesting in a type tag string.
pub const MAX_ARRAY_DEPTH: usize = 4;

const SLIP_END: u8 = 0xC0;
const SLIP_ESC: u8 = 0xDB;
const SLIP_ESC_END: u8 = 0xDC;
const SLIP_ESC_ESC: u8 = 0xDD;
const BUNDLE_TAG: &[u8; 8] = b"#bundle\0";

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("empty packet")]
    Empty,
    #[error("packet of {len} bytes is over the {MAX_PACKET_BYTES}-byte limit")]
    TooLarge { len: usize },
    #[error("bundles nested deeper than {MAX_BUNDLE_DEPTH}")]
    BundleTooDeep,
    #[error("more than {MAX_MESSAGES_PER_PACKET} messages in one packet")]
    TooManyMessages,
    #[error("bad bundle: {0}")]
    BadBundle(&'static str),
    #[error("type tags nest arrays deeper than {MAX_ARRAY_DEPTH}")]
    TagsTooDeep,
    #[error("bad message: {0}")]
    BadMessage(String),
}

/// Decodes one datagram or SLIP frame into its messages, in order. A bundle is flattened; its
/// time tags are ignored (messages run on arrival). Total: any bytes give messages or an error.
pub fn decode_packet(bytes: &[u8]) -> Result<Vec<OscMessage>, DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::Empty);
    }
    if bytes.len() > MAX_PACKET_BYTES {
        return Err(DecodeError::TooLarge { len: bytes.len() });
    }
    let mut out = Vec::new();
    flatten(bytes, 0, &mut out)?;
    Ok(out)
}

fn flatten(bytes: &[u8], depth: usize, out: &mut Vec<OscMessage>) -> Result<(), DecodeError> {
    if !bytes.starts_with(BUNDLE_TAG) {
        if out.len() >= MAX_MESSAGES_PER_PACKET {
            return Err(DecodeError::TooManyMessages);
        }
        out.push(decode_message(bytes)?);
        return Ok(());
    }
    let depth = depth + 1;
    if depth > MAX_BUNDLE_DEPTH {
        return Err(DecodeError::BundleTooDeep);
    }
    // 8 bytes of tag, 8 of time tag, then size-prefixed elements.
    let mut rest = bytes
        .get(16..)
        .ok_or(DecodeError::BadBundle("shorter than its header"))?;
    while !rest.is_empty() {
        let Some((size, after)) = rest.split_first_chunk::<4>() else {
            return Err(DecodeError::BadBundle("truncated element size"));
        };
        let len = usize::try_from(u32::from_be_bytes(*size))
            .map_err(|_| DecodeError::BadBundle("element size"))?;
        let Some((element, after)) = after.split_at_checked(len) else {
            return Err(DecodeError::BadBundle("element longer than its bundle"));
        };
        if element.is_empty() {
            return Err(DecodeError::BadBundle("empty element"));
        }
        flatten(element, depth, out)?;
        rest = after;
    }
    Ok(())
}

fn decode_message(bytes: &[u8]) -> Result<OscMessage, DecodeError> {
    check_tag_depth(bytes)?;
    match rosc::decoder::decode_udp(bytes) {
        Ok((_, OscPacket::Message(message))) => Ok(message),
        // Cannot happen (bundles are walked above); refuse rather than recurse into one.
        Ok((_, OscPacket::Bundle(_))) => Err(DecodeError::BadMessage("unexpected bundle".into())),
        Err(e) => Err(DecodeError::BadMessage(e.to_string())),
    }
}

/// Rejects a type tag string whose `[` nest too deep. A message too short to have tags is left
/// for `rosc` to refuse.
fn check_tag_depth(bytes: &[u8]) -> Result<(), DecodeError> {
    let Some(address_end) = bytes.iter().position(|&b| b == 0) else {
        return Ok(());
    };
    let tags_start = (address_end + 1).next_multiple_of(4);
    let Some(tags) = bytes.get(tags_start..) else {
        return Ok(());
    };
    let mut depth = 0usize;
    for &b in tags.iter().take_while(|&&b| b != 0) {
        match b {
            b'[' => {
                depth += 1;
                if depth > MAX_ARRAY_DEPTH {
                    return Err(DecodeError::TagsTooDeep);
                }
            }
            b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("could not encode OSC: {0}")]
pub struct EncodeError(String);

/// One message as a UDP datagram (and the payload of a SLIP frame).
pub fn encode_message(message: &OscMessage) -> Result<Vec<u8>, EncodeError> {
    rosc::encoder::encode(&OscPacket::Message(message.clone()))
        .map_err(|e| EncodeError(e.to_string()))
}

/// Wraps `payload` as one SLIP frame (OSC 1.1 over a stream): END on both ends, so a receiver
/// that joined mid-noise resynchronises on the first one.
pub fn slip_encode(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 2);
    out.push(SLIP_END);
    for &b in payload {
        match b {
            SLIP_END => out.extend_from_slice(&[SLIP_ESC, SLIP_ESC_END]),
            SLIP_ESC => out.extend_from_slice(&[SLIP_ESC, SLIP_ESC_ESC]),
            other => out.push(other),
        }
    }
    out.push(SLIP_END);
    out
}

/// What a stream decoder found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlipEvent {
    /// One complete, unescaped frame (never empty).
    Frame(Vec<u8>),
    /// A frame passed the size limit. Reported once, as soon as it does; the rest of the frame
    /// is skipped up to the next END, without being buffered.
    Oversize,
    /// A frame had an escape byte not followed by an escaped END or ESC; the frame is dropped.
    BadEscape,
}

/// Incremental SLIP decoder, one per TCP connection. Feed it whatever each read returns; frames
/// may be split across any number of reads. Memory is bounded by the frame limit.
#[derive(Debug)]
pub struct SlipDecoder {
    buf: Vec<u8>,
    max_frame: usize,
    escaped: bool,
    bad: bool,
    skipping: bool,
}

impl Default for SlipDecoder {
    fn default() -> Self {
        Self::new(MAX_PACKET_BYTES)
    }
}

impl SlipDecoder {
    pub fn new(max_frame: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame,
            escaped: false,
            bad: false,
            skipping: false,
        }
    }

    /// Appends the events found in `data` to `events`.
    pub fn feed(&mut self, data: &[u8], events: &mut Vec<SlipEvent>) {
        for &byte in data {
            if self.skipping {
                if byte == SLIP_END {
                    self.reset_frame();
                }
                continue;
            }
            match (byte, self.escaped) {
                (SLIP_END, _) => self.end_frame(events),
                (SLIP_ESC, false) => self.escaped = true,
                (SLIP_ESC_END, true) => self.push(SLIP_END, events),
                (SLIP_ESC_ESC, true) => self.push(SLIP_ESC, events),
                // ESC followed by anything else (including ESC): the frame is bad, and is
                // skipped to its END.
                (_, true) => {
                    self.bad = true;
                    self.escaped = false;
                }
                (other, false) => self.push(other, events),
            }
        }
    }

    fn push(&mut self, byte: u8, events: &mut Vec<SlipEvent>) {
        self.escaped = false;
        if self.buf.len() >= self.max_frame {
            self.skipping = true;
            self.buf = Vec::new();
            events.push(SlipEvent::Oversize);
        } else {
            self.buf.push(byte);
        }
    }

    fn end_frame(&mut self, events: &mut Vec<SlipEvent>) {
        // An END right after ESC ends a frame whose escape was cut short.
        if self.bad || self.escaped {
            events.push(SlipEvent::BadEscape);
        } else if !self.buf.is_empty() {
            events.push(SlipEvent::Frame(std::mem::take(&mut self.buf)));
        }
        self.reset_frame();
    }

    fn reset_frame(&mut self) {
        self.buf = Vec::new();
        self.escaped = false;
        self.bad = false;
        self.skipping = false;
    }
}
