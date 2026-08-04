// SPDX-License-Identifier: Apache-2.0
//! Automatic emitter discovery across extension unit controls.

use crate::descriptors::ExtensionUnit;
use crate::emitter::{verify_emitter, EmitterEvidence, EmitterVerifyCfg};
use crate::probe::{SafeXu, XuError, XuTransport};
use namtor_capture::frame::Frame;

/// Supplies frames for verification. Implemented by the live capture path and
/// by test fakes.
pub trait FrameSampler {
    fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError>;
}

/// A control that measurably drove the emitter.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub unit: u8,
    pub selector: u8,
    pub payload_on: Vec<u8>,
    pub payload_off: Vec<u8>,
    pub evidence: EmitterEvidence,
}

#[derive(Debug, Clone)]
pub struct DiscoverCfg {
    /// Control selectors to try. UVC selectors are 1-based and rarely exceed 16.
    pub selectors: Vec<u8>,
    /// Frames to average per measurement.
    pub sample_frames: usize,
    pub verify: EmitterVerifyCfg,
}

impl Default for DiscoverCfg {
    fn default() -> Self {
        Self {
            selectors: (0x01..=0x10).collect(),
            sample_frames: 4,
            verify: EmitterVerifyCfg::default(),
        }
    }
}

/// Candidate payloads to try for a control of the given length.
///
/// Ordered by likelihood: a single set bit is the most common encoding for a
/// boolean emitter enable, followed by all-ones.
fn candidate_payloads(len: usize) -> Vec<Vec<u8>> {
    let mut one = vec![0u8; len];
    if len > 0 {
        one[0] = 0x01;
    }
    vec![one, vec![0xffu8; len]]
}

/// Searches extension unit controls for one that drives the IR emitter.
///
/// Every write is preceded by a successful read and followed by a restore,
/// per the probe safety rules. Controls without SET support are skipped.
pub fn discover_emitter<T: XuTransport, S: FrameSampler>(
    xu: &mut SafeXu<T>,
    sampler: &mut S,
    units: &[ExtensionUnit],
    cfg: &DiscoverCfg,
) -> Vec<Candidate> {
    let mut found = Vec::new();

    for unit in units {
        // Try selectors the unit actually advertises before sweeping blindly.
        // On the reference camera the IR MSXU advertises 6 and 9, so this
        // finds the emitter in two queries instead of sixteen.
        let mut ordered = unit.advertised_selectors();
        for &s in &cfg.selectors {
            if !ordered.contains(&s) {
                ordered.push(s);
            }
        }

        for selector in ordered {
            let Ok(profile) = xu.probe_control(unit.unit_id, selector) else {
                continue; // control absent; not an error
            };
            if !profile.info.supports_set() || profile.len == 0 {
                continue;
            }

            let Ok(off_frames) = sampler.sample(cfg.sample_frames) else {
                continue;
            };

            for payload in candidate_payloads(profile.len) {
                if payload == profile.current {
                    continue; // no state change to observe
                }

                let Ok(restorer) = xu.try_write(unit.unit_id, selector, &payload) else {
                    continue;
                };

                let on_frames = sampler.sample(cfg.sample_frames).unwrap_or_default();
                drop(restorer); // restores the prior value

                let evidence = verify_emitter(&off_frames, &on_frames, &cfg.verify);
                if evidence.confident {
                    found.push(Candidate {
                        unit: unit.unit_id,
                        selector,
                        payload_on: payload,
                        payload_off: profile.current.clone(),
                        evidence,
                    });
                    break; // this control is identified; move on
                }
            }
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{SafeXu, XuTransport};
    use crate::xu::Query;
    use namtor_capture::frame::{EmitterState, PixFmt};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::time::Duration;

    type ControlMap = HashMap<(u8, u8), (u16, u8, Vec<u8>)>;

    /// A fake camera in which exactly one control drives brightness.
    ///
    /// Shared through `Rc` so the transport and the frame sampler observe the
    /// *same* device state — otherwise the positive test cannot succeed and
    /// the negative test proves nothing.
    struct FakeCamera {
        controls: RefCell<ControlMap>,
        magic: (u8, u8),
    }

    impl FakeCamera {
        fn new(magic: (u8, u8)) -> Self {
            let mut m = HashMap::new();
            // Two writable controls and one read-only control.
            m.insert((3, 0x01), (1u16, 0b11u8, vec![0x00]));
            m.insert((3, 0x02), (1u16, 0b11u8, vec![0x00]));
            m.insert((3, 0x03), (1u16, 0b01u8, vec![0x00]));
            Self {
                controls: RefCell::new(m),
                magic,
            }
        }

        fn brightness(&self) -> u8 {
            let c = self.controls.borrow();
            match c.get(&self.magic) {
                Some((_, _, v)) if v.iter().any(|&b| b != 0) => 200,
                _ => 40,
            }
        }
    }

    impl XuTransport for Rc<FakeCamera> {
        fn query(
            &self,
            unit: u8,
            selector: u8,
            query: Query,
            buf: &mut [u8],
        ) -> Result<(), XuError> {
            let mut c = self.controls.borrow_mut();
            let Some((len, info, value)) = c.get_mut(&(unit, selector)) else {
                return Err(XuError::Ioctl("ENOENT".into()));
            };
            match query {
                Query::GetLen => buf[0] = *len as u8,
                Query::GetInfo => buf[0] = *info,
                Query::GetCur => buf.copy_from_slice(value),
                Query::SetCur => value.copy_from_slice(buf),
                _ => return Err(XuError::Ioctl("unsupported".into())),
            }
            Ok(())
        }
    }

    /// Samples frames whose brightness reflects the shared camera's state.
    struct FakeSampler {
        cam: Rc<FakeCamera>,
    }

    impl FrameSampler for FakeSampler {
        fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError> {
            let level = self.cam.brightness();
            Ok((0..n)
                .map(|i| {
                    // Centre-weighted so the radial check passes.
                    let (w, h) = (64usize, 64usize);
                    let mut data = vec![0u8; w * h];
                    for y in 0..h {
                        for x in 0..w {
                            let dx = x as f64 - 32.0;
                            let dy = y as f64 - 32.0;
                            let r = (dx * dx + dy * dy).sqrt() / 45.25;
                            let base = 40.0;
                            let extra = (level as f64 - 40.0) / (1.0 + 3.0 * r * r);
                            data[y * w + x] = (base + extra).min(255.0) as u8;
                        }
                    }
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

    fn unit(unit_id: u8) -> ExtensionUnit {
        ExtensionUnit {
            interface: 2,
            unit_id,
            guid: [0u8; 16],
            bm_controls: vec![0xff],
        }
    }

    #[test]
    fn finds_the_control_that_changes_brightness() {
        let cam = Rc::new(FakeCamera::new((3, 0x02)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };

        let found = discover_emitter(&mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default());

        assert_eq!(found.len(), 1, "exactly one control drives brightness");
        assert_eq!(found[0].unit, 3);
        assert_eq!(found[0].selector, 0x02);
        assert!(found[0].evidence.confident);
        assert!(found[0].evidence.delta > 10.0);
    }

    #[test]
    fn the_discovered_payloads_round_trip_the_control() {
        let cam = Rc::new(FakeCamera::new((3, 0x02)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };

        let found = discover_emitter(&mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default());

        // payload_off must be the value the control held before probing, so
        // the quirks entry can restore a known-good state.
        assert_eq!(found[0].payload_off, vec![0x00]);
        assert!(found[0].payload_on.iter().any(|&b| b != 0));
    }

    #[test]
    fn the_control_is_left_in_its_original_state_after_discovery() {
        // Probe safety rule 2, observed end to end.
        let cam = Rc::new(FakeCamera::new((3, 0x02)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };

        discover_emitter(&mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default());

        assert_eq!(cam.brightness(), 40, "emitter must be off after probing");
    }

    #[test]
    fn read_only_controls_are_never_written() {
        // Selector 0x03 has info 0b01 (no SET support), so probe safety rule
        // 4 must keep it out of the results even though it is the magic
        // control that would change brightness.
        let cam = Rc::new(FakeCamera::new((3, 0x03)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = FakeSampler {
            cam: Rc::clone(&cam),
        };

        let found = discover_emitter(&mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default());

        assert!(found.iter().all(|c| c.selector != 0x03));
    }

    /// Sampler returning a constant level, for negative tests.
    struct ConstSampler {
        level: u8,
    }

    impl FrameSampler for ConstSampler {
        fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError> {
            Ok((0..n)
                .map(|i| Frame {
                    data: vec![self.level; 64 * 64],
                    format: PixFmt::Grey8,
                    width: 64,
                    height: 64,
                    ts_mono: Duration::from_millis(33 * i as u64),
                    sof: None,
                    emitter: EmitterState::Unknown,
                    quality: None,
                })
                .collect())
        }
    }

    #[test]
    fn a_camera_with_no_brightness_response_yields_no_candidates() {
        // The magic control does not exist, so nothing changes brightness.
        let cam = Rc::new(FakeCamera::new((9, 0x09)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = ConstSampler { level: 40 };

        let found = discover_emitter(&mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default());
        assert!(
            found.is_empty(),
            "must not report an emitter that isn't there"
        );
    }

    #[test]
    fn unknown_units_are_skipped_without_error() {
        let cam = Rc::new(FakeCamera::new((3, 0x02)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = ConstSampler { level: 40 };

        let found = discover_emitter(&mut xu, &mut sampler, &[unit(99)], &DiscoverCfg::default());
        assert!(found.is_empty());
    }
}
