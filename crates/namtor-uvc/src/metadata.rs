// SPDX-License-Identifier: Apache-2.0
//! UVC payload header metadata, from the `V4L2_META_FMT_UVC` node.
//!
//! This is what makes frame-accurate strobe pairing possible: extension unit
//! writes are USB control transfers with no frame synchronisation, so a
//! toggle can land mid-exposure. SOF and PTS let torn frames be identified
//! and discarded rather than silently corrupting the differential.

use thiserror::Error;

/// Fixed portion of `struct uvc_meta_buf`: u64 ns, u16 sof, u8 length, u8 flags.
const META_FIXED_LEN: usize = 12;

/// bmHeaderInfo bit 1: this payload ends a frame.
const FLAG_END_OF_FRAME: u8 = 1 << 1;
/// bmHeaderInfo bit 2: a 4-byte PTS follows.
const FLAG_PTS: u8 = 1 << 2;
/// bmHeaderInfo bit 6: the device flagged an error on this payload.
const FLAG_ERROR: u8 = 1 << 6;

#[derive(Debug, Error, PartialEq)]
pub enum MetaError {
    #[error("metadata buffer too short: {0} bytes, need at least {META_FIXED_LEN}")]
    TooShort(usize),
}

/// One parsed `V4L2_META_FMT_UVC` buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct UvcMetaBuf {
    /// Kernel monotonic timestamp when the payload was received.
    pub ns: u64,
    /// USB start-of-frame counter.
    pub sof: u16,
    /// Length of the UVC payload header.
    pub length: u8,
    /// The header's `bmHeaderInfo`.
    pub flags: u8,
    /// Header bytes following the fixed fields.
    pub payload: Vec<u8>,
}

impl UvcMetaBuf {
    pub fn end_of_frame(&self) -> bool {
        self.flags & FLAG_END_OF_FRAME != 0
    }

    /// True when the device flagged an error. Such frames are discarded.
    pub fn has_error(&self) -> bool {
        self.flags & FLAG_ERROR != 0
    }

    /// Presentation timestamp, when present and complete.
    ///
    /// Returns `None` rather than panicking if the device sets the flag but
    /// truncates the bytes — vendors do this.
    pub fn pts(&self) -> Option<u32> {
        if self.flags & FLAG_PTS == 0 || self.payload.len() < 4 {
            return None;
        }
        Some(u32::from_le_bytes([
            self.payload[0],
            self.payload[1],
            self.payload[2],
            self.payload[3],
        ]))
    }
}

pub fn parse_meta(buf: &[u8]) -> Result<UvcMetaBuf, MetaError> {
    if buf.len() < META_FIXED_LEN {
        return Err(MetaError::TooShort(buf.len()));
    }
    Ok(UvcMetaBuf {
        ns: u64::from_ne_bytes(buf[0..8].try_into().expect("checked length")),
        sof: u16::from_ne_bytes(buf[8..10].try_into().expect("checked length")),
        length: buf[10],
        flags: buf[11],
        payload: buf[META_FIXED_LEN..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `uvc_meta_buf` with the given flags and trailing payload.
    fn meta(ns: u64, sof: u16, flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&ns.to_ne_bytes());
        v.extend_from_slice(&sof.to_ne_bytes());
        v.push(2 + payload.len() as u8); // UVC header length
        v.push(flags);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_timestamp_and_start_of_frame() {
        let raw = meta(1_234_567_890, 4242, 0b0000_0010, &[]);
        let m = parse_meta(&raw).unwrap();
        assert_eq!(m.ns, 1_234_567_890);
        assert_eq!(m.sof, 4242);
        assert!(m.end_of_frame());
    }

    #[test]
    fn extracts_pts_when_the_flag_is_set() {
        // bit 2 set: PTS present, four bytes little-endian.
        let raw = meta(0, 1, 0b0000_0100, &0xDEAD_BEEFu32.to_le_bytes());
        let m = parse_meta(&raw).unwrap();
        assert_eq!(m.pts(), Some(0xDEAD_BEEF));
    }

    #[test]
    fn returns_no_pts_when_the_flag_is_clear() {
        let raw = meta(0, 1, 0b0000_0010, &[]);
        assert_eq!(parse_meta(&raw).unwrap().pts(), None);
    }

    #[test]
    fn returns_no_pts_when_the_flag_is_set_but_bytes_are_missing() {
        // A device that lies about its own header must not cause a panic.
        let raw = meta(0, 1, 0b0000_0100, &[0x01, 0x02]);
        assert_eq!(parse_meta(&raw).unwrap().pts(), None);
    }

    #[test]
    fn surfaces_the_error_bit() {
        // bit 6: error. Such frames must never enter the differential.
        let raw = meta(0, 1, 0b0100_0000, &[]);
        assert!(parse_meta(&raw).unwrap().has_error());
    }

    #[test]
    fn rejects_buffers_shorter_than_the_fixed_header() {
        assert!(parse_meta(&[0u8; 5]).is_err());
    }
}
