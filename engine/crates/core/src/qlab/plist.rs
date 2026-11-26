// SPDX-License-Identifier: GPL-3.0-or-later

//! A lazy reader of Apple's binary property lists (`bplist00`). Nothing is parsed up front:
//! [`Plist::object`] reads one object by its index from the offset table when it is asked for,
//! so memory stays at the size of the input and a hostile file costs at most the read budget.
//! Every length, offset and reference is checked against the input; nothing indexes unchecked.

use std::cell::Cell;

use super::QlabError;

/// Longest string read, in bytes (a UTF-16 string counts two bytes a unit).
pub(super) const MAX_STRING_BYTES: usize = 1024 * 1024;

/// How many object reads one import may make, over every archive it opens. A file can make a
/// reader visit shared objects many times; this bounds the work whatever the file holds.
pub(super) const MAX_READS: u64 = 64_000_000;

/// The read budget shared by every [`Plist`] of one import.
pub(super) struct Budget(Cell<u64>);

impl Budget {
    pub(super) fn new(reads: u64) -> Self {
        Self(Cell::new(reads))
    }

    fn spend(&self) -> Result<(), QlabError> {
        let left = self.0.get();
        if left == 0 {
            return Err(QlabError::TooComplex);
        }
        self.0.set(left - 1);
        Ok(())
    }
}

/// One object of a property list. Strings, data and the members of collections are borrowed
/// from the input; references are object indexes, read with [`Plist::object`].
#[derive(Clone, Copy, Debug)]
pub(super) enum Value<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Data(&'a [u8]),
    /// One byte a character.
    Ascii(&'a [u8]),
    /// Big-endian UTF-16 code units.
    Utf16(&'a [u8]),
    /// A keyed archive's reference to one of its `$objects`.
    Uid(u64),
    Array(Refs<'a>),
    Dict {
        keys: Refs<'a>,
        values: Refs<'a>,
    },
}

/// A run of object references.
#[derive(Clone, Copy, Debug)]
pub(super) struct Refs<'a> {
    bytes: &'a [u8],
    size: usize,
}

impl<'a> Refs<'a> {
    pub(super) fn len(&self) -> usize {
        self.bytes.len() / self.size.max(1)
    }

    pub(super) fn get(&self, i: usize) -> Option<u64> {
        let start = i.checked_mul(self.size)?;
        let end = start.checked_add(self.size)?;
        self.bytes.get(start..end).map(be_uint)
    }

    pub(super) fn iter(self) -> impl Iterator<Item = u64> + 'a {
        self.bytes.chunks_exact(self.size.max(1)).map(be_uint)
    }
}

/// A big-endian unsigned number of 1 to 8 bytes.
fn be_uint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b))
}

/// An opened binary property list.
pub(super) struct Plist<'a> {
    data: &'a [u8],
    offsets: Refs<'a>,
    ref_size: usize,
    top: u64,
    budget: &'a Budget,
}

impl<'a> Plist<'a> {
    /// Checks the header and trailer. The objects are read later, one at a time.
    pub(super) fn open(data: &'a [u8], budget: &'a Budget) -> Result<Self, QlabError> {
        if !data.starts_with(b"bplist00") || data.len() < 8 + 32 {
            return Err(QlabError::NotPlist);
        }
        let trailer = data
            .get(data.len() - 32..)
            .ok_or(QlabError::Damaged("trailer"))?;
        let field = |range: std::ops::Range<usize>| trailer.get(range).map(be_uint);
        let offset_size = field(6..7).unwrap_or(0) as usize;
        let ref_size = field(7..8).unwrap_or(0) as usize;
        let count = field(8..16).ok_or(QlabError::Damaged("trailer"))?;
        let top = field(16..24).ok_or(QlabError::Damaged("trailer"))?;
        let table = field(24..32).ok_or(QlabError::Damaged("trailer"))?;
        if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) {
            return Err(QlabError::Damaged("trailer sizes"));
        }
        // Every object is at least one byte before the table.
        let body = data.len() - 32;
        let table = usize::try_from(table).map_err(|_| QlabError::Damaged("offset table"))?;
        let count = usize::try_from(count).map_err(|_| QlabError::Damaged("object count"))?;
        if count == 0 || count > table || table > body || top >= count as u64 {
            return Err(QlabError::Damaged("object count"));
        }
        let table_len = count
            .checked_mul(offset_size)
            .ok_or(QlabError::Damaged("offset table"))?;
        let table_end = table
            .checked_add(table_len)
            .ok_or(QlabError::Damaged("offset table"))?;
        let offsets = data
            .get(table..table_end)
            .filter(|_| table_end <= body)
            .ok_or(QlabError::Damaged("offset table"))?;
        Ok(Self {
            data: data
                .get(..table)
                .ok_or(QlabError::Damaged("offset table"))?,
            offsets: Refs {
                bytes: offsets,
                size: offset_size,
            },
            ref_size,
            top,
            budget,
        })
    }

    /// The index of the top object.
    pub(super) fn top(&self) -> u64 {
        self.top
    }

    /// Reads object `index`.
    pub(super) fn object(&self, index: u64) -> Result<Value<'a>, QlabError> {
        self.budget.spend()?;
        let index = usize::try_from(index).map_err(|_| QlabError::Damaged("reference"))?;
        let offset = self
            .offsets
            .get(index)
            .ok_or(QlabError::Damaged("reference"))?;
        let at = usize::try_from(offset).map_err(|_| QlabError::Damaged("offset"))?;
        let marker = *self.data.get(at).ok_or(QlabError::Damaged("offset"))?;
        let (kind, low) = (marker >> 4, marker & 0x0f);
        let after = at + 1;
        match kind {
            0x0 => match low {
                0x0 => Ok(Value::Null),
                0x8 => Ok(Value::Bool(false)),
                0x9 => Ok(Value::Bool(true)),
                _ => Err(QlabError::Damaged("marker")),
            },
            0x1 => {
                let bytes = self.slice(after, 1usize << low.min(4))?;
                match bytes.len() {
                    1 | 2 | 4 => Ok(Value::Int(be_uint(bytes) as i64)),
                    // Eight bytes are signed; sixteen hold a 128-bit number whose top half
                    // this reader only accepts as a sign.
                    8 => Ok(Value::Int(be_uint(bytes) as i64)),
                    16 => {
                        let low = bytes.get(8..).map(be_uint).unwrap_or(0);
                        Ok(Value::Int(low as i64))
                    }
                    _ => Err(QlabError::Damaged("integer")),
                }
            }
            0x2 => match low {
                2 => {
                    let b = self.slice(after, 4)?;
                    let bits = u32::try_from(be_uint(b)).unwrap_or(0);
                    Ok(Value::Real(f64::from(f32::from_bits(bits))))
                }
                3 => Ok(Value::Real(f64::from_bits(be_uint(self.slice(after, 8)?)))),
                _ => Err(QlabError::Damaged("real")),
            },
            0x3 if low == 3 => Ok(Value::Real(f64::from_bits(be_uint(self.slice(after, 8)?)))),
            0x4 => {
                let (len, start) = self.length(low, after)?;
                Ok(Value::Data(self.slice(start, len)?))
            }
            0x5 => {
                let (len, start) = self.length(low, after)?;
                if len > MAX_STRING_BYTES {
                    return Err(QlabError::TooLarge("string"));
                }
                Ok(Value::Ascii(self.slice(start, len)?))
            }
            0x6 => {
                let (len, start) = self.length(low, after)?;
                let bytes = len.checked_mul(2).ok_or(QlabError::Damaged("string"))?;
                if bytes > MAX_STRING_BYTES {
                    return Err(QlabError::TooLarge("string"));
                }
                Ok(Value::Utf16(self.slice(start, bytes)?))
            }
            0x8 => Ok(Value::Uid(be_uint(
                self.slice(after, usize::from(low) + 1)?,
            ))),
            0xA | 0xC => {
                let (len, start) = self.length(low, after)?;
                Ok(Value::Array(self.refs(start, len)?))
            }
            0xD => {
                let (len, start) = self.length(low, after)?;
                let keys = self.refs(start, len)?;
                let values_at = start
                    .checked_add(len.saturating_mul(self.ref_size))
                    .ok_or(QlabError::Damaged("dictionary"))?;
                Ok(Value::Dict {
                    keys,
                    values: self.refs(values_at, len)?,
                })
            }
            _ => Err(QlabError::Damaged("marker")),
        }
    }

    fn slice(&self, start: usize, len: usize) -> Result<&'a [u8], QlabError> {
        let end = start.checked_add(len).ok_or(QlabError::Damaged("length"))?;
        self.data
            .get(start..end)
            .ok_or(QlabError::Damaged("length"))
    }

    fn refs(&self, start: usize, len: usize) -> Result<Refs<'a>, QlabError> {
        let bytes = len
            .checked_mul(self.ref_size)
            .ok_or(QlabError::Damaged("collection"))?;
        Ok(Refs {
            bytes: self.slice(start, bytes)?,
            size: self.ref_size,
        })
    }

    /// The length a marker's low nibble gives, or the integer object after it when the nibble
    /// is 0xF; and where the content starts.
    fn length(&self, low: u8, after: usize) -> Result<(usize, usize), QlabError> {
        if low != 0x0f {
            return Ok((usize::from(low), after));
        }
        let marker = *self.data.get(after).ok_or(QlabError::Damaged("length"))?;
        if marker >> 4 != 0x1 || marker & 0x0f > 3 {
            return Err(QlabError::Damaged("length"));
        }
        let size = 1usize << (marker & 0x0f);
        let start = after + 1;
        let len = be_uint(self.slice(start, size)?);
        let len = usize::try_from(len).map_err(|_| QlabError::Damaged("length"))?;
        // A collection or string cannot be longer than the input it sits in.
        if len > self.data.len() {
            return Err(QlabError::Damaged("length"));
        }
        Ok((len, start + size))
    }
}

/// The text of a string object, at most [`MAX_STRING_BYTES`] long; `None` for any other value.
pub(super) fn text(value: Value<'_>) -> Option<String> {
    match value {
        Value::Ascii(bytes) => Some(bytes.iter().map(|&b| char::from(b)).collect()),
        Value::Utf16(bytes) => {
            let units = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_be_bytes(*p));
            Some(
                char::decode_utf16(units)
                    .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect(),
            )
        }
        _ => None,
    }
}

/// Whether `value` is the ASCII string `want` (no allocation).
pub(super) fn is_ascii(value: Value<'_>, want: &str) -> bool {
    matches!(value, Value::Ascii(bytes) if bytes == want.as_bytes())
}
