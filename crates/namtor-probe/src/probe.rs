// SPDX-License-Identifier: Apache-2.0
//! Tiered emitter probing: MSXU silent → vendor consented.
//!
//! Two tiers, matching spec §4.10.1:
//!
//! - **Tier 1 (MSXU, silent):** standardised semantics, so the probe tries
//!   every mode the control advertises without asking. G1.4 — zero
//!   interaction on MSXU — is structural: the consent gate is never consulted
//!   in this tier.
//! - **Tier 2 (vendor XU, consented):** a blind write on a vendor unit is
//!   genuinely uncontrolled, so the probe asks the consent gate first. The
//!   gate decides whether to prompt; the probe only learns yes/no.
//!
//! Reuses M1 `namtor-uvc` primitives: `SafeXu` enforces probe safety rule
//! for-rule, `find_mode_controls` / `mode_candidates` drive the MSXU tier,
//! `discover_emitter` drives the vendor tier, and `measure_alternation`
//! classifies a captured burst.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use namtor_capture::frame::{EmitterState, Frame, PixFmt};
use namtor_hwdb::quirks::EmitterMethod;
use namtor_uvc::descriptors::ExtensionUnit;
use namtor_uvc::device::V4l2Device;
use namtor_uvc::discover::{discover_emitter, DiscoverCfg, FrameSampler};
use namtor_uvc::mode::{find_mode_controls, measure_alternation};
use namtor_uvc::probe::{SafeXu, XuError, XuTransport};
use serde::{Deserialize, Serialize};

/// Decides whether to attempt a vendor-XU probe. The probe never prompts
/// directly; it asks this gate, so the policy (CLI flag, polkit, config) lives
/// behind one trait.
pub trait ConsentGate {
    fn request_vendor_probe(&self, vid: u16, pid: u16) -> bool;
}

/// Outcome of a probe run.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ProbeResult {
    pub vid_pid: String,
    pub method: EmitterMethod,
    pub unit: u8,
    pub selector: u8,
    pub payload_on: Vec<u8>,
    pub payload_off: Vec<u8>,
    pub mode: Option<u8>,
    /// Human-readable evidence summary; contains no biometric data.
    pub evidence: String,
}

impl ProbeResult {
    /// A `ProbeResult` describing a device whose emitter could not be driven.
    pub fn unknown(vid: u16, pid: u16, reason: &str) -> Self {
        Self {
            vid_pid: format!("{vid:04x}:{pid:04x}"),
            method: EmitterMethod::Unknown,
            unit: 0,
            selector: 0,
            payload_on: Vec::new(),
            payload_off: Vec::new(),
            mode: None,
            evidence: reason.to_string(),
        }
    }
}

/// Runs the tiered probe against a device's IR extension units.
///
/// `ir_xus` must already be filtered to the IR VideoControl interface and
/// ordered MSXU-first (as `namtor-hwdb::xu_index::enumerate_xus` produces).
/// The MSXU tier is tried silently; the vendor tier only if no MSXU mode
/// alternated and the consent gate approves.
pub fn run_probe<T: XuTransport, S: FrameSampler>(
    xu: &mut SafeXu<T>,
    sampler: &mut S,
    ir_xus: &[ExtensionUnit],
    vid: u16,
    pid: u16,
    consent: &dyn ConsentGate,
) -> ProbeResult {
    let msxu: Vec<&ExtensionUnit> = ir_xus.iter().filter(|x| is_msxu(x)).collect();
    let vendor: Vec<&ExtensionUnit> = ir_xus.iter().filter(|x| !is_msxu(x)).collect();

    // Tier 1: MSXU mode sweep. Silent — consent never consulted (G1.4).
    if let Some(hit) = probe_msxu_modes(xu, sampler, &msxu) {
        return ProbeResult {
            vid_pid: format!("{vid:04x}:{pid:04x}"),
            method: EmitterMethod::MsxuMode,
            unit: hit.unit,
            selector: hit.selector,
            payload_on: hit.payload_on,
            payload_off: hit.payload_off,
            mode: Some(hit.mode),
            evidence: hit.evidence,
        };
    }

    // Tier 2: vendor XU discovery, consented.
    if consent.request_vendor_probe(vid, pid) {
        let owned: Vec<ExtensionUnit> = vendor.into_iter().cloned().collect();
        let found = discover_emitter(xu, sampler, &owned, &DiscoverCfg::default());
        if let Some(c) = found.into_iter().next() {
            return ProbeResult {
                vid_pid: format!("{vid:04x}:{pid:04x}"),
                method: EmitterMethod::VendorXu,
                unit: c.unit,
                selector: c.selector,
                payload_on: c.payload_on,
                payload_off: c.payload_off,
                mode: None,
                evidence: format!(
                    "vendor XU delta {:.1}, centre_weighted {}",
                    c.evidence.delta, c.evidence.centre_weighted
                ),
            };
        }
        return ProbeResult::unknown(vid, pid, "vendor probe consented but found no emitter");
    }

    ProbeResult::unknown(vid, pid, "no MSXU alternation and vendor probe declined")
}

fn is_msxu(xu: &ExtensionUnit) -> bool {
    namtor_hwdb::xu_index::is_msxu(&xu.guid)
}

struct MsxuHit {
    unit: u8,
    selector: u8,
    payload_on: Vec<u8>,
    payload_off: Vec<u8>,
    mode: u8,
    evidence: String,
}

/// Sweeps every mode candidate of every MSXU mode control, returning the first
/// that produces firmware alternation. The `Restorer` returned by each
/// `try_write` puts the original value back on drop, so the control is left
/// in its default state regardless of outcome.
fn probe_msxu_modes<T: XuTransport, S: FrameSampler>(
    xu: &mut SafeXu<T>,
    sampler: &mut S,
    msxu: &[&ExtensionUnit],
) -> Option<MsxuHit> {
    let owned: Vec<ExtensionUnit> = msxu.iter().map(|x| (*x).clone()).collect();
    let controls = find_mode_controls(xu, &owned);

    for ctrl in &controls {
        let mode_idx = ctrl.mode_byte();
        for payload in ctrl.mode_candidates() {
            let Ok(restorer) = xu.try_write(ctrl.unit, ctrl.selector, &payload) else {
                continue;
            };
            let frames = sampler
                .sample(DiscoverCfg::default().sample_frames)
                .unwrap_or_default();
            drop(restorer); // restore default before deciding

            let ev = measure_alternation(&frames);
            if ev.alternates {
                let mode_byte = mode_idx
                    .map(|i| payload.get(i).copied().unwrap_or(0))
                    .unwrap_or(0);
                return Some(MsxuHit {
                    unit: ctrl.unit,
                    selector: ctrl.selector,
                    payload_on: payload,
                    payload_off: ctrl.default.clone(),
                    mode: mode_byte,
                    evidence: format!(
                        "MSXU mode {mode_byte} alternation {:.1} (on {:.1} / off {:.1})",
                        ev.alternation, ev.on_mean, ev.off_mean
                    ),
                });
            }
        }
    }

    None
}

/// Samples frames by shelling out to `v4l2-ctl --stream-mmap`.
///
/// Reused from the M1 spike: most UVC devices support only mmap streaming, so
/// `read()` would fail. M2's in-process `IrUvc` (Task 12) will eventually
/// replace this, but it is not on the critical path for the probe CLI.
pub struct V4l2CtlSampler {
    device: std::path::PathBuf,
    width: u32,
    height: u32,
    started: Instant,
    scratch: std::path::PathBuf,
}

impl V4l2CtlSampler {
    pub fn new(device: impl Into<std::path::PathBuf>, width: u32, height: u32) -> Self {
        Self {
            device: device.into(),
            width,
            height,
            started: Instant::now(),
            scratch: std::env::temp_dir().join("namtor-probe-frames.raw"),
        }
    }
}

impl FrameSampler for V4l2CtlSampler {
    fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError> {
        let fmt = format!(
            "width={},height={},pixelformat=GREY",
            self.width, self.height
        );
        let out = Command::new("v4l2-ctl")
            .arg("-d")
            .arg(&self.device)
            .arg(format!("--set-fmt-video={fmt}"))
            .arg("--stream-mmap")
            .arg(format!("--stream-count={n}"))
            .arg(format!("--stream-to={}", self.scratch.display()))
            .output()
            .map_err(|e| XuError::Ioctl(format!("spawn v4l2-ctl: {e}")))?;

        if !out.status.success() {
            return Err(XuError::Ioctl(format!(
                "v4l2-ctl failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }

        let raw = std::fs::read(&self.scratch)
            .map_err(|e| XuError::Ioctl(format!("read capture: {e}")))?;
        let len = (self.width * self.height) as usize;

        if raw.len() < len {
            return Err(XuError::Ioctl(format!(
                "captured {} bytes, need at least {len} for one frame",
                raw.len()
            )));
        }

        Ok(raw
            .chunks_exact(len)
            .take(n)
            .enumerate()
            .map(|(i, chunk)| Frame {
                data: chunk.to_vec(),
                format: PixFmt::Grey8,
                width: self.width,
                height: self.height,
                ts_mono: self.started.elapsed() + Duration::from_millis(33 * i as u64),
                sof: None,
                emitter: EmitterState::Unknown,
                quality: None,
            })
            .collect())
    }
}

/// Live probe wrapper: opens the V4L2 device, builds a v4l2-ctl sampler, and
/// runs the tiered probe against the IR extension units. Hardware-gated.
pub fn run_probe_live(
    device: &Path,
    ir_xus: &[ExtensionUnit],
    vid: u16,
    pid: u16,
    consent: &dyn ConsentGate,
) -> Result<ProbeResult, XuError> {
    let transport = V4l2Device::open(device)?;
    let mut xu = SafeXu::new(transport);
    let mut sampler = V4l2CtlSampler::new(device, 640, 360);
    Ok(run_probe(&mut xu, &mut sampler, ir_xus, vid, pid, consent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_uvc::xu::Query;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::Duration;

    /// (unit, selector) -> (len, info byte, value)
    type ControlMap = HashMap<(u8, u8), (u16, u8, Vec<u8>)>;

    /// A fake camera whose MSXU mode control produces alternation when set to
    /// mode 2, and whose vendor XU has a magic brightness control. State is
    /// shared via `Rc` so the transport and sampler observe the same device.
    struct FakeCamera {
        controls: RefCell<ControlMap>,
        /// (unit, selector) of the MSXU mode control.
        msxu_mode: (u8, u8),
        /// (unit, selector) of the vendor brightness control, if any.
        vendor_magic: Option<(u8, u8)>,
    }

    impl FakeCamera {
        /// Reference-shaped MSXU: 9-byte control, CUR mode 1, MAX mode 3.
        /// Mode 2 (set via `try_write`) makes the sampler return alternation.
        fn new(msxu_mode: (u8, u8), vendor_magic: Option<(u8, u8)>) -> Self {
            let mut m = HashMap::new();
            // MSXU mode control: selectors advertised by bmControls 0x20 (bit 5
            // -> selector 6). info 0b11 (GET+SET). 9-byte payload; byte 2 is
            // the mode field. CUR = mode 1 (emitter off), MAX = mode 3.
            let cur = vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0];
            let max = vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0];
            m.insert(msxu_mode, (9, 0b11, cur.clone()));
            // Stash MAX via a synthetic key so GET_MAX can return it; we
            // intercept GetMax below.
            m.insert((msxu_mode.0, msxu_mode.1 | 0x80), (9, 0b11, max));
            m.insert((msxu_mode.0, msxu_mode.1 | 0x40), (9, 0b11, cur)); // DEF

            if let Some(vm) = vendor_magic {
                m.insert(vm, (1, 0b11, vec![0x00]));
            }
            Self {
                controls: RefCell::new(m),
                msxu_mode,
                vendor_magic,
            }
        }

        fn current_mode(&self) -> u8 {
            let c = self.controls.borrow();
            c.get(&self.msxu_mode)
                .map(|(_, _, v)| v.get(2).copied().unwrap_or(0))
                .unwrap_or(0)
        }

        fn brightness(&self) -> u8 {
            let c = self.controls.borrow();
            match &self.vendor_magic {
                Some(vm) => match c.get(vm) {
                    Some((_, _, v)) if v.iter().any(|&b| b != 0) => 200,
                    _ => 40,
                },
                None => 40,
            }
        }
    }

    /// Newtype transport wrapper so we can impl the foreign `XuTransport`
    /// trait on a local type (orphan rule: `Rc` alone is not fundamental).
    struct Cam(Rc<FakeCamera>);

    impl XuTransport for Cam {
        fn query(
            &self,
            unit: u8,
            selector: u8,
            query: Query,
            buf: &mut [u8],
        ) -> Result<(), XuError> {
            // Map GetMax/GetDef to the synthetic keys stashed at creation.
            let key = match query {
                Query::GetMax => (unit, selector | 0x80),
                Query::GetDef => (unit, selector | 0x40),
                _ => (unit, selector),
            };
            let mut c = self.0.controls.borrow_mut();
            let entry = if c.contains_key(&key) {
                c.get_mut(&key)
            } else {
                c.get_mut(&(unit, selector))
            };
            let Some((len, info, value)) = entry else {
                return Err(XuError::Ioctl("ENOENT".into()));
            };
            match query {
                Query::GetLen => {
                    buf[0] = (*len & 0xff) as u8;
                    if buf.len() > 1 {
                        buf[1] = (*len >> 8) as u8;
                    }
                }
                Query::GetInfo => buf[0] = *info,
                Query::GetCur => buf.copy_from_slice(value),
                Query::GetMax | Query::GetDef => buf.copy_from_slice(value),
                Query::SetCur => {
                    value.copy_from_slice(buf);
                }
                _ => return Err(XuError::Ioctl("unsupported".into())),
            }
            Ok(())
        }
    }

    /// Sampler that returns alternating frames when the MSXU mode is 2, and
    /// steady frames otherwise. Also reflects vendor brightness if set.
    struct FakeSampler {
        cam: Rc<FakeCamera>,
    }

    impl FrameSampler for FakeSampler {
        fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError> {
            let mode = self.cam.current_mode();
            let vendor_level = self.cam.brightness();
            Ok((0..n)
                .map(|i| {
                    let (w, h) = (64usize, 64usize);
                    let mut data = vec![0u8; w * h];
                    let level = if mode == 2 {
                        if i % 2 == 0 {
                            200
                        } else {
                            0
                        }
                    } else if vendor_level > 40 {
                        // Centre-weighted so verify_emitter's radial check passes.
                        for y in 0..h {
                            for x in 0..w {
                                let dx = x as f64 - 32.0;
                                let dy = y as f64 - 32.0;
                                let r = (dx * dx + dy * dy).sqrt() / 45.25;
                                let base = 40.0;
                                let extra = (vendor_level as f64 - 40.0) / (1.0 + 3.0 * r * r);
                                data[y * w + x] = (base + extra).min(255.0) as u8;
                            }
                        }
                        return Frame {
                            data,
                            format: PixFmt::Grey8,
                            width: w as u32,
                            height: h as u32,
                            ts_mono: Duration::from_millis(33 * i as u64),
                            sof: None,
                            emitter: EmitterState::Unknown,
                            quality: None,
                        };
                    } else {
                        40
                    };
                    data.fill(level);
                    Frame {
                        data,
                        format: PixFmt::Grey8,
                        width: w as u32,
                        height: h as u32,
                        ts_mono: Duration::from_millis(33 * i as u64),
                        sof: None,
                        emitter: EmitterState::Unknown,
                        quality: None,
                    }
                })
                .collect())
        }
    }

    /// Consent gate that records every call and returns a fixed answer.
    struct AutoConsent {
        allow: bool,
        calls: RefCell<u32>,
    }

    impl AutoConsent {
        fn new(allow: bool) -> Self {
            Self {
                allow,
                calls: RefCell::new(0),
            }
        }
        fn calls(&self) -> u32 {
            *self.calls.borrow()
        }
    }

    impl ConsentGate for AutoConsent {
        fn request_vendor_probe(&self, _vid: u16, _pid: u16) -> bool {
            *self.calls.borrow_mut() += 1;
            self.allow
        }
    }

    /// An MSXU XU with bmControls advertising selector 6.
    fn msxu_unit(unit_id: u8) -> ExtensionUnit {
        let mut guid = [0u8; 16];
        // 0f3f95dc-2632-4c4e-... MSXU prefix.
        guid[..4].copy_from_slice(&[0xdc, 0x95, 0x3f, 0x0f]);
        guid[4..8].copy_from_slice(&[0x32, 0x26, 0x4e, 0x4c]);
        ExtensionUnit {
            interface: 2,
            unit_id,
            guid,
            bm_controls: vec![0x20], // bit 5 -> selector 6
        }
    }

    /// A vendor XU with bmControls advertising selectors 1 and 2.
    fn vendor_unit(unit_id: u8) -> ExtensionUnit {
        ExtensionUnit {
            interface: 2,
            unit_id,
            guid: [0xff; 16],        // not MSXU
            bm_controls: vec![0x03], // selectors 1, 2
        }
    }

    #[test]
    fn msxu_tier_succeeds_without_consulting_consent() {
        // (a): MSXU mode 2 alternates; consent must never be asked.
        let cam = Rc::new(FakeCamera::new((4, 6), None));
        let mut xu = SafeXu::new(Cam(Rc::clone(&cam)));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };
        let consent = AutoConsent::new(true);

        let result = run_probe(
            &mut xu,
            &mut sampler,
            &[msxu_unit(4)],
            0x3277,
            0x0018,
            &consent,
        );

        assert_eq!(result.method, EmitterMethod::MsxuMode);
        assert_eq!(result.mode, Some(2));
        assert_eq!(result.unit, 4);
        assert_eq!(result.selector, 6);
        assert_eq!(consent.calls(), 0, "G1.4: MSXU tier must not ask consent");
    }

    #[test]
    fn no_msxu_alternation_and_consent_denied_yields_unknown() {
        // (b): MSXU present but no mode alternates (sampler never sees mode 2
        // because CUR is mode 1 and candidates skip it... but here the fake's
        // sampler returns steady frames for any mode except 2, and mode 2 IS a
        // candidate). To make this test meaningful, point the magic mode at a
        // selector the fake does NOT light, so no mode alternates.
        let cam = Rc::new(FakeCamera::new((4, 9), Some((3, 0x02))));
        let mut xu = SafeXu::new(Cam(Rc::clone(&cam)));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };
        let consent = AutoConsent::new(false);

        let result = run_probe(
            &mut xu,
            &mut sampler,
            &[msxu_unit(4), vendor_unit(3)],
            0x3277,
            0x0018,
            &consent,
        );

        assert_eq!(result.method, EmitterMethod::Unknown);
        assert_eq!(consent.calls(), 1, "consent asked once before declining");
    }

    #[test]
    fn no_msxu_alternation_and_consent_allowed_yields_vendor_xu() {
        // (c): MSXU inert, consent allowed, vendor discover_emitter finds the
        // brightness control.
        let cam = Rc::new(FakeCamera::new((4, 9), Some((3, 0x02))));
        let mut xu = SafeXu::new(Cam(Rc::clone(&cam)));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };
        let consent = AutoConsent::new(true);

        let result = run_probe(
            &mut xu,
            &mut sampler,
            &[msxu_unit(4), vendor_unit(3)],
            0x3277,
            0x0018,
            &consent,
        );

        assert_eq!(result.method, EmitterMethod::VendorXu);
        assert_eq!(result.unit, 3);
        assert_eq!(consent.calls(), 1);
    }
}
