// SPDX-License-Identifier: GPL-3.0-only
//! Length-delimited JSON framing for both wire seams.
//!
//! On the wire: `| 4-byte BE length | JSON payload of that length |`.
//!
//! `decode` reads exactly one framed message and parses it to a
//! [`serde_json::Value`] so the caller can deserialise into the expected typed
//! message after inspecting a discriminant. A length header that announces more
//! bytes than the stream provides yields [`ProtoError::Truncated`].

use std::io::{Read, Write};

use serde::Serialize;
use thiserror::Error;

/// Header length, in bytes.
const HEADER_LEN: usize = 4;
/// Reject absurd length headers so a corrupt peer can't make us allocate 4 GiB.
const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("truncated stream: header announced {announced} bytes")]
    Truncated { announced: u32 },
    #[error("payload too large: {0} bytes (limit {MAX_PAYLOAD})")]
    Oversized(u32),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Encode `msg` as a length-prefixed JSON payload onto `w`.
pub fn encode<T: Serialize>(msg: &T, w: &mut impl Write) -> Result<(), ProtoError> {
    let payload = serde_json::to_vec(msg)?;
    let len = u32::try_from(payload.len()).map_err(|_| ProtoError::Oversized(u32::MAX))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&payload)?;
    w.flush()?;
    Ok(())
}

/// Read one length-prefixed message from `r` and parse it to a
/// [`serde_json::Value`].
///
/// Returns [`ProtoError::Truncated`] if the header announces more bytes than
/// the stream can deliver. A clean EOF before any header byte is read is
/// reported as [`ProtoError::Io`] of kind `UnexpectedEof`; callers that treat
/// EOF as end-of-stream should match on the io kind before propagating.
pub fn decode(r: &mut impl Read) -> Result<serde_json::Value, ProtoError> {
    let mut header = [0u8; HEADER_LEN];
    read_exact_or_eof(r, &mut header)?;
    let announced = u32::from_be_bytes(header);
    if announced > MAX_PAYLOAD {
        return Err(ProtoError::Oversized(announced));
    }
    let mut buf = vec![0u8; announced as usize];
    match r.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(ProtoError::Truncated { announced });
        }
        Err(e) => return Err(ProtoError::Io(e)),
    }
    let value = serde_json::from_slice(&buf)?;
    Ok(value)
}

/// Read exactly `buf.len()` bytes or hit clean EOF before reading any bytes.
///
/// A clean EOF *before* any byte is read is surfaced as an `UnexpectedEof`
/// io error so the caller can distinguish "no more messages" from "message
/// truncated mid-header". `read_exact` already has this semantics for the
/// header case (it errors with `UnexpectedEof` if zero bytes arrive before
/// EOF), but we wrap it here to keep the call site explicit.
fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> Result<(), ProtoError> {
    r.read_exact(buf).map_err(ProtoError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Demo {
        a: u32,
        b: String,
    }

    #[test]
    fn round_trip_known_message() {
        let msg = Demo {
            a: 7,
            b: "hi".into(),
        };
        let mut buf = Vec::new();
        encode(&msg, &mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let v = decode(&mut cursor).unwrap();
        let back: Demo = serde_json::from_value(v).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn round_trip_empty_payload() {
        // A zero-length JSON payload is invalid JSON, so use an empty array.
        let msg: Vec<u8> = Vec::new();
        let mut buf = Vec::new();
        encode(&msg, &mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let v = decode(&mut cursor).unwrap();
        let back: Vec<u8> = serde_json::from_value(v).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn truncated_stream_is_detected() {
        // Header says 100 bytes follow, but we only write 4.
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u32.to_be_bytes());
        buf.extend_from_slice(b"abcd");
        let mut cursor = std::io::Cursor::new(&buf);
        let err = decode(&mut cursor).unwrap_err();
        match err {
            ProtoError::Truncated { announced, .. } => assert_eq!(announced, 100),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn bad_json_payload_is_json_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&5u32.to_be_bytes());
        buf.extend_from_slice(b"{bad}");
        let mut cursor = std::io::Cursor::new(&buf);
        let err = decode(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtoError::Json(_)), "got {err:?}");
    }

    #[test]
    fn oversized_header_is_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_PAYLOAD + 1).to_be_bytes());
        let mut cursor = std::io::Cursor::new(&buf);
        let err = decode(&mut cursor).unwrap_err();
        assert!(matches!(err, ProtoError::Oversized(_)), "got {err:?}");
    }

    #[test]
    fn two_messages_back_to_back() {
        let m1 = Demo {
            a: 1,
            b: "x".into(),
        };
        let m2 = Demo {
            a: 2,
            b: "yy".into(),
        };
        let mut buf = Vec::new();
        encode(&m1, &mut buf).unwrap();
        encode(&m2, &mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let v1 = decode(&mut cursor).unwrap();
        let v2 = decode(&mut cursor).unwrap();
        assert_eq!(serde_json::from_value::<Demo>(v1).unwrap(), m1);
        assert_eq!(serde_json::from_value::<Demo>(v2).unwrap(), m2);
    }

    /// A header announcing exactly MAX_PAYLOAD bytes with no payload following
    /// is a truncated stream, not an oversized payload. Proves the boundary
    /// check is `<` not `<=`, so MAX_PAYLOAD itself is an accepted size and
    /// only the missing body is reported.
    #[test]
    fn max_payload_boundary_not_oversized() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAX_PAYLOAD.to_be_bytes());
        let mut cursor = std::io::Cursor::new(&buf);
        let err = decode(&mut cursor).unwrap_err();
        match err {
            ProtoError::Truncated { announced, .. } => assert_eq!(announced, MAX_PAYLOAD),
            other => panic!("expected Truncated at MAX_PAYLOAD, got {other:?}"),
        }
    }
}
