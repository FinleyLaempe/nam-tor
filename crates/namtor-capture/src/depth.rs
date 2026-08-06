// SPDX-License-Identifier: GPL-3.0-only
//! Capability-gated depth capture path.
//!
//! The reference hardware (ASUS ROG Flow X16) has no depth sensor, and most
//! Windows Hello-class laptops share that limitation. Depth is therefore an
//! *optional* liveness channel: when present it strengthens PAD, when absent
//! the strobe-differential IR path carries authentication alone.
//!
//! This module exposes capability discovery, quality assessment, and a PAD
//! veto that **fails closed** — an unsupported or malformed depth stream never
//! returns [`DepthVetoResult::DepthPass`]. Production use of the depth path is
//! gated by [`DepthCapability::production_ready`], which stays `false` until a
//! hardware-in-the-loop (HIL) campaign records measured APCER per attack type
//! (spec §6.4, R10). Until then the auth path falls back to IR via
//! [`ensure_ir_fallback`].

use crate::frame::{Frame, PixFmt};
use crate::source::{CaptureSource, SourceCaps, Spectrum};
use serde::{Deserialize, Serialize};

/// Depth pixel formats Nam-tor can carry. `Depth16` is the V4L2 `DEPTH16` code
/// (millimetres, little-endian u16 per pixel). `DepthZ16` is the depth-in-Z
/// variant some Realsense nodes expose. Anything else is unsupported and the
/// depth path must not engage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepthFormat {
    Depth16,
    DepthZ16,
    Unsupported,
}

/// A [`CaptureSource`] that additionally reports depth geometry.
///
/// Implementors are depth-bearing devices (e.g. Realsense-class hardware).
/// The auth path never assumes a depth source exists; it discovers one via
/// [`discover_depth`] and only downcasts when the capability is present.
pub trait DepthSource: CaptureSource {
    /// Pixel format of the depth stream.
    fn depth_format(&self) -> DepthFormat;
    /// `(min, max)` depth range in millimetres the device reports reliably.
    fn depth_range_mm(&self) -> (u16, u16);
}

/// Discovered depth capability for a capture node. Produced by
/// [`discover_depth`] from static [`SourceCaps`]; carries enough state for the
/// auth path to decide whether to engage the depth veto or fall back to IR.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DepthCapability {
    pub available: bool,
    pub format: DepthFormat,
    pub min_range_mm: u16,
    pub max_range_mm: u16,
}

impl DepthCapability {
    /// Production readiness gate for the depth PAD path.
    ///
    /// Returns `false` until a hardware-in-the-loop campaign records measured
    /// APCER per attack type for the depth veto (spec §6.4, R10). The auth
    /// path must call this before trusting [`depth_pad_veto`] and fall back to
    /// IR-only via [`ensure_ir_fallback`] when it returns `false`.
    ///
    /// Flip `DEPTH_HIL_VALIDATED` to `true` only after the HIL evidence lands
    /// in `docs/evidence/`.
    pub fn production_ready(&self) -> bool {
        // Compile-time gate. Stays false until HIL evidence is recorded.
        const DEPTH_HIL_VALIDATED: bool = false;
        self.available && DEPTH_HIL_VALIDATED
    }
}

/// Default range assumed for a Depth16 stream when the device reports none.
/// Covers the face-auth envelope (0.3–1.2 m); wider ranges still validate but
/// quality assessment may downgrade them.
const DEFAULT_DEPTH_RANGE_MM: (u16, u16) = (300, 1200);

/// Inspects static [`SourceCaps`] and reports the depth capability.
///
/// Only `Spectrum::Depth` streams carrying a known depth pixel format are
/// considered available. Anything else — including RGB-only and IR-only nodes
/// — returns `available: false` with [`DepthFormat::Unsupported`].
pub fn discover_depth(caps: &SourceCaps) -> DepthCapability {
    let format = match (caps.spectrum, caps.format) {
        (Spectrum::Depth, PixFmt::Depth16) => DepthFormat::Depth16,
        // Z16 is not yet a first-class PixFmt; treat Depth16 spectrum as
        // Depth16 until a distinct code is introduced.
        _ => DepthFormat::Unsupported,
    };
    match format {
        DepthFormat::Unsupported => DepthCapability {
            available: false,
            format,
            min_range_mm: 0,
            max_range_mm: 0,
        },
        _ => DepthCapability {
            available: true,
            format,
            min_range_mm: DEFAULT_DEPTH_RANGE_MM.0,
            max_range_mm: DEFAULT_DEPTH_RANGE_MM.1,
        },
    }
}

/// Quality band for a depth frame sequence, used to weight or veto fusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthQuality {
    Good,
    Marginal,
    Poor,
    NoDepth,
}

/// Minimum fraction of valid (non-zero, non-saturated) pixels for `Good`.
const GOOD_VALID_FRAC: f32 = 0.50;
/// Minimum fraction of valid pixels for `Marginal`. Below this is `Poor`.
const MARGINAL_VALID_FRAC: f32 = 0.20;

/// Assesses a depth frame sequence by the fraction of valid pixels.
///
/// A pixel is *valid* when its u16 depth is non-zero (no return) and not
/// saturated (`0xFFFF`). Frames that fail [`Frame::validate`] or are not
/// `Depth16` contribute zero valid pixels. An empty slice or a slice with no
/// depth frames yields [`DepthQuality::NoDepth`].
pub fn assess_depth_quality(frames: &[Frame]) -> DepthQuality {
    let mut total_pixels: usize = 0;
    let mut valid_pixels: usize = 0;
    for f in frames {
        if f.format != PixFmt::Depth16 || f.validate().is_err() {
            continue;
        }
        let pixels = f.width as usize * f.height as usize;
        if pixels == 0 {
            continue;
        }
        total_pixels += pixels;
        let u16s = bytemuck_cast_u16(&f.data);
        for &v in u16s {
            if v != 0 && v != u16::MAX {
                valid_pixels += 1;
            }
        }
    }
    if total_pixels == 0 {
        return DepthQuality::NoDepth;
    }
    let frac = valid_pixels as f32 / total_pixels as f32;
    if frac >= GOOD_VALID_FRAC {
        DepthQuality::Good
    } else if frac >= MARGINAL_VALID_FRAC {
        DepthQuality::Marginal
    } else {
        DepthQuality::Poor
    }
}

/// Outcome of the depth PAD veto. The veto is a *deny* term: only
/// [`DepthVetoResult::DepthPass`] permits the depth path to corroborate
/// liveness. [`DepthVetoResult::DepthUnsupported`] signals "no depth channel;
/// fall back to IR" — it is not a pass. [`DepthVetoResult::DepthFail`] means a
/// depth channel was present but the liveness signal was absent or malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthVetoResult {
    DepthPass,
    DepthFail(String),
    DepthUnsupported,
}

/// Depth PAD veto (spec §6.4 depth corroboration).
///
/// Fails closed:
/// - No depth frames, or frames not in `Depth16` format, or any frame failing
///   [`Frame::validate`] → [`DepthVetoResult::DepthUnsupported`] when no
///   usable depth is present, or [`DepthVetoResult::DepthFail`] when depth is
///   present but malformed.
/// - Quality [`DepthQuality::Poor`] or [`DepthQuality::NoDepth`] →
///   [`DepthVetoResult::DepthFail`].
/// - Only [`DepthQuality::Good`] or [`DepthQuality::Marginal`] with at least
///   one IR frame present can return [`DepthVetoResult::DepthPass`].
///
/// The IR slice is required because depth-only authentication is not in scope;
/// depth corroborates the IR strobe-differential path, it does not replace it.
pub fn depth_pad_veto(depth: &[Frame], ir: &[Frame]) -> DepthVetoResult {
    if depth.is_empty() {
        return DepthVetoResult::DepthUnsupported;
    }
    // Any malformed depth frame fails closed rather than being ignored.
    let depth_present = depth.iter().any(|f| f.format == PixFmt::Depth16);
    if !depth_present {
        return DepthVetoResult::DepthUnsupported;
    }
    for f in depth {
        if f.format == PixFmt::Depth16 && f.validate().is_err() {
            return DepthVetoResult::DepthFail("malformed depth frame".to_string());
        }
    }
    if ir.is_empty() {
        return DepthVetoResult::DepthFail("missing IR corroboration".to_string());
    }
    match assess_depth_quality(depth) {
        DepthQuality::Good | DepthQuality::Marginal => DepthVetoResult::DepthPass,
        DepthQuality::Poor => DepthVetoResult::DepthFail("depth quality too low".to_string()),
        DepthQuality::NoDepth => DepthVetoResult::DepthFail("no usable depth pixels".to_string()),
    }
}

/// Decides whether the auth path should fall back to IR-only.
///
/// Returns `true` when depth is unavailable or not yet production-ready (HIL
/// gate, see [`DepthCapability::production_ready`]). The caller must use this
/// before engaging [`depth_pad_veto`] so that an unvalidated depth path never
/// silently weakens the strobe-differential PAD decision.
pub fn ensure_ir_fallback(cap: &DepthCapability) -> bool {
    !cap.production_ready()
}

/// Cast a byte slice to a u16 slice without an external dependency. Returns an
/// empty slice when the length is odd (defensive; the caller has already
/// validated the frame, so this is unreachable in practice).
fn bytemuck_cast_u16(bytes: &[u8]) -> &[u16] {
    if !bytes.len().is_multiple_of(2) {
        return &[];
    }
    // SAFETY: u16 is `Copy` and has no alignment requirements beyond 2, which
    // the byte slice satisfies for any even length. The slice is borrowed from
    // `bytes` which outlives the returned reference.
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u16, bytes.len() / 2) }
}

/// Replay fixture for depth-frame tests. Holds synthetic `Depth16` frames with
/// a configurable valid-pixel fraction so tests can drive each quality band.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct DepthFixture {
    pub frames: Vec<Frame>,
}

#[cfg(test)]
impl DepthFixture {
    /// Builds `count` depth frames of `width × height` where `valid_frac` of
    /// pixels are non-zero, non-saturated u16 depths.
    pub fn valid_frac(width: u32, height: u32, count: usize, valid_frac: f32) -> Self {
        let pixels = width as usize * height as usize;
        let bytes_per_frame = pixels * 2;
        let valid_count = (pixels as f32 * valid_frac.clamp(0.0, 1.0)) as usize;
        let mut frames = Vec::with_capacity(count);
        for i in 0..count {
            let mut data = vec![0u8; bytes_per_frame];
            let u16s: &mut [u16] =
                // SAFETY: even-length buffer, 2-byte aligned by vec allocation.
                unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u16, pixels) };
            for v in u16s.iter_mut().take(valid_count) {
                // 500 mm ~ face distance; varies slightly to avoid saturation.
                *v = 500u16.saturating_add((i % 50) as u16);
            }
            frames.push(Frame {
                data,
                format: PixFmt::Depth16,
                width,
                height,
                ts_mono: std::time::Duration::from_millis(33 * i as u64),
                sof: Some(i as u16),
                emitter: crate::frame::EmitterState::Unknown,
                quality: None,
            });
        }
        Self { frames }
    }

    /// Builds a depth frame whose buffer length is wrong for its geometry,
    /// used to prove the veto fails closed on malformed input.
    pub fn malformed(width: u32, height: u32) -> Self {
        let pixels = width as usize * height as usize;
        // Half the required length → validate() fails.
        let data = vec![0u8; pixels];
        Self {
            frames: vec![Frame {
                data,
                format: PixFmt::Depth16,
                width,
                height,
                ts_mono: std::time::Duration::from_millis(0),
                sof: None,
                emitter: crate::frame::EmitterState::Unknown,
                quality: None,
            }],
        }
    }

    /// Builds `count` grey8 IR frames for corroboration in veto tests.
    pub fn ir_corroboration(width: u32, height: u32, count: usize) -> Vec<Frame> {
        let len = (width as usize) * (height as usize);
        (0..count)
            .map(|i| Frame {
                data: vec![128u8; len],
                format: PixFmt::Grey8,
                width,
                height,
                ts_mono: std::time::Duration::from_millis(33 * i as u64),
                sof: Some(i as u16),
                emitter: crate::frame::EmitterState::On,
                quality: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth_caps() -> SourceCaps {
        SourceCaps {
            spectrum: Spectrum::Depth,
            format: PixFmt::Depth16,
            width: 640,
            height: 360,
            fps: 30,
            has_metadata: false,
        }
    }

    fn rgb_caps() -> SourceCaps {
        SourceCaps {
            spectrum: Spectrum::Rgb,
            format: PixFmt::Yuyv,
            width: 1280,
            height: 720,
            fps: 30,
            has_metadata: false,
        }
    }

    #[test]
    fn discover_depth_finds_depth_format() {
        let cap = discover_depth(&depth_caps());
        assert!(cap.available);
        assert_eq!(cap.format, DepthFormat::Depth16);
        assert!(cap.min_range_mm > 0);
        assert!(cap.max_range_mm > cap.min_range_mm);
    }

    #[test]
    fn discover_depth_returns_unsupported_for_rgb_only() {
        let cap = discover_depth(&rgb_caps());
        assert!(!cap.available);
        assert_eq!(cap.format, DepthFormat::Unsupported);
        assert_eq!(cap.min_range_mm, 0);
        assert_eq!(cap.max_range_mm, 0);
    }

    #[test]
    fn assess_depth_quality_good_marginal_poor() {
        let good = DepthFixture::valid_frac(64, 48, 2, 0.75);
        assert_eq!(assess_depth_quality(&good.frames), DepthQuality::Good);
        let marginal = DepthFixture::valid_frac(64, 48, 2, 0.30);
        assert_eq!(
            assess_depth_quality(&marginal.frames),
            DepthQuality::Marginal
        );
        let poor = DepthFixture::valid_frac(64, 48, 2, 0.05);
        assert_eq!(assess_depth_quality(&poor.frames), DepthQuality::Poor);
        assert_eq!(assess_depth_quality(&[]), DepthQuality::NoDepth);
    }

    #[test]
    fn depth_pad_veto_fails_closed_on_unsupported() {
        // Empty depth → unsupported (no depth channel to engage).
        let ir = DepthFixture::ir_corroboration(64, 48, 2);
        assert_eq!(depth_pad_veto(&[], &ir), DepthVetoResult::DepthUnsupported);
        // Non-depth frames → unsupported.
        let non_depth = vec![Frame {
            data: vec![0u8; 64 * 48],
            format: PixFmt::Grey8,
            width: 64,
            height: 48,
            ts_mono: std::time::Duration::from_millis(0),
            sof: None,
            emitter: crate::frame::EmitterState::On,
            quality: None,
        }];
        assert_eq!(
            depth_pad_veto(&non_depth, &ir),
            DepthVetoResult::DepthUnsupported
        );
    }

    #[test]
    fn depth_pad_veto_passes_on_valid_depth() {
        let depth = DepthFixture::valid_frac(64, 48, 2, 0.75);
        let ir = DepthFixture::ir_corroboration(64, 48, 2);
        assert_eq!(
            depth_pad_veto(&depth.frames, &ir),
            DepthVetoResult::DepthPass
        );
    }

    #[test]
    fn ir_fallback_preserves_auth_path() {
        // Depth unavailable → IR fallback engaged, auth path preserved.
        let cap = discover_depth(&rgb_caps());
        assert!(ensure_ir_fallback(&cap));
        // Depth available but HIL not validated → still fall back to IR.
        let cap = discover_depth(&depth_caps());
        assert!(ensure_ir_fallback(&cap));
    }

    #[test]
    fn production_ready_false_without_hil() {
        let cap = discover_depth(&depth_caps());
        assert!(!cap.production_ready());
        // Even an unavailable capability must not be production-ready.
        let cap = discover_depth(&rgb_caps());
        assert!(!cap.production_ready());
    }

    #[test]
    fn malformed_depth_fails_closed() {
        let malformed = DepthFixture::malformed(64, 48);
        let ir = DepthFixture::ir_corroboration(64, 48, 2);
        let res = depth_pad_veto(&malformed.frames, &ir);
        assert!(matches!(res, DepthVetoResult::DepthFail(_)));
        // Poor quality (but valid geometry) also fails closed, not passes.
        let poor = DepthFixture::valid_frac(64, 48, 2, 0.02);
        let res = depth_pad_veto(&poor.frames, &ir);
        assert!(matches!(res, DepthVetoResult::DepthFail(_)));
        // Missing IR corroboration fails closed even with good depth.
        let good = DepthFixture::valid_frac(64, 48, 2, 0.75);
        let res = depth_pad_veto(&good.frames, &[]);
        assert!(matches!(res, DepthVetoResult::DepthFail(_)));
    }

    /// A `DepthSource` implementor must satisfy both `CaptureSource` and the
    /// depth trait surface. This proves the trait is implementable.
    #[test]
    fn depth_source_trait_is_implementable() {
        struct FakeDepth;
        impl CaptureSource for FakeDepth {
            fn caps(&self) -> SourceCaps {
                depth_caps()
            }
            fn start(&mut self) -> Result<(), crate::source::CaptureError> {
                Ok(())
            }
            fn next_frame(&mut self) -> Result<Option<Frame>, crate::source::CaptureError> {
                Ok(None)
            }
            fn stop(&mut self) {}
        }
        impl DepthSource for FakeDepth {
            fn depth_format(&self) -> DepthFormat {
                DepthFormat::Depth16
            }
            fn depth_range_mm(&self) -> (u16, u16) {
                (300, 1200)
            }
        }
        let mut d = FakeDepth;
        assert_eq!(d.depth_format(), DepthFormat::Depth16);
        assert_eq!(d.depth_range_mm(), (300, 1200));
        assert!(d.start().is_ok());
    }
}
