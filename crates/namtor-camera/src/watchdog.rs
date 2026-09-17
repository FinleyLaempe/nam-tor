// SPDX-License-Identifier: GPL-3.0-only
//! Emitter watchdog and `ExecStopPost` teardown helper.
//!
//! Spec §4.10.2: the emitter must not stay on indefinitely. A watchdog arms
//! when the emitter is turned on and forces it off after `max_on_ms` (default
//! 2000 from quirks). The `namtor emitter-off` subcommand is a thin
//! `ExecStopPost=` target for the systemd unit: best-effort force-off on
//! unexpected teardown.

use std::path::Path;
use std::time::{Duration, Instant};

use namtor_hwdb::quirks::EmitterCfg;
use namtor_uvc::device::V4l2Device;
use namtor_uvc::probe::{SafeXu, XuTransport};

/// Abstraction over the clock so the watchdog is deterministic in tests.
pub trait Clock {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

pub struct MockClock {
    pub t: std::cell::Cell<Instant>,
}

impl MockClock {
    pub fn new(start: Instant) -> Self {
        Self {
            t: std::cell::Cell::new(start),
        }
    }
    pub fn advance(&self, d: Duration) {
        let new = self.t.get() + d;
        self.t.set(new);
    }
}

impl Clock for MockClock {
    fn now(&self) -> Instant {
        self.t.get()
    }
}

/// Arms on `apply_on()`, fires if the emitter stays on past `max_on_ms`.
pub struct Watchdog<'a, C: Clock> {
    max_on_ms: u32,
    clock: &'a C,
    armed_at: Option<Instant>,
}

impl<'a, C: Clock> Watchdog<'a, C> {
    pub fn new(max_on_ms: u32, clock: &'a C) -> Self {
        Self {
            max_on_ms,
            clock,
            armed_at: None,
        }
    }

    pub fn from_quirks(cfg: &EmitterCfg, clock: &'a C) -> Self {
        let max = cfg.lifecycle.as_ref().map(|l| l.max_on_ms).unwrap_or(2000);
        Self::new(max, clock)
    }

    pub fn arm(&mut self) {
        self.armed_at = Some(self.clock.now());
    }

    pub fn disarm(&mut self) {
        self.armed_at = None;
    }

    pub fn is_armed(&self) -> bool {
        self.armed_at.is_some()
    }

    /// True if the emitter has been on longer than `max_on_ms`. The caller
    /// is responsible for forcing the emitter off when this returns true.
    pub fn check_expired(&self) -> bool {
        match self.armed_at {
            Some(t0) => {
                self.clock.now().duration_since(t0) > Duration::from_millis(self.max_on_ms as u64)
            }
            None => false,
        }
    }
}

/// Library-level force-off used by the `namtor emitter-off` subcommand.
/// Best-effort: logs errors, never panics.
pub fn force_off_lib<T: XuTransport>(xu: &mut SafeXu<T>, cfg: &EmitterCfg) -> Result<(), String> {
    let unit = cfg.unit.ok_or("emitter config missing unit")?;
    let selector = cfg.selector.ok_or("emitter config missing selector")?;
    let payload = &cfg.payload_off;
    if xu.probe_control(unit, selector).is_err() {
        return Ok(());
    }
    match xu.try_write(unit, selector, payload) {
        Ok(restorer) => {
            restorer.keep();
            Ok(())
        }
        Err(e) => Err(format!("force_off write failed: {e}")),
    }
}

/// Opens a V4L2 device, builds a `SafeXu`, and forces the emitter off.
/// Called by the `namtor emitter-off` subcommand on `ExecStopPost`.
pub fn force_off_on_device(device: &Path, cfg: &EmitterCfg) -> Result<(), String> {
    let dev = V4l2Device::open(device).map_err(|e| format!("open {}: {e}", device.display()))?;
    let mut xu = SafeXu::new(dev);
    force_off_lib(&mut xu, cfg)
}

/// Checks the watchdog and, if expired, forces the emitter off and disarms.
/// Returns `true` when a force-off was performed, `false` otherwise. Used by
/// the daemon's per-frame loop to enforce `max_on_ms` without a separate
/// timer thread.
pub fn enforce_off_if_expired<C: Clock, T: XuTransport>(
    wd: &mut Watchdog<'_, C>,
    xu: &mut SafeXu<T>,
    cfg: &EmitterCfg,
) -> Result<bool, String> {
    if wd.check_expired() {
        force_off_lib(xu, cfg)?;
        wd.disarm();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_uvc::probe::{SafeXu, XuTransport};
    use namtor_uvc::xu::Query;
    use std::cell::RefCell;
    use std::collections::HashMap;

    type ControlMap = HashMap<(u8, u8), (u16, u8, Vec<u8>)>;

    struct FakeDevice {
        controls: RefCell<ControlMap>,
    }
    impl FakeDevice {
        fn new() -> Self {
            Self {
                controls: RefCell::new(HashMap::new()),
            }
        }
        fn add(&self, unit: u8, sel: u8, info: u8, value: Vec<u8>) {
            self.controls
                .borrow_mut()
                .insert((unit, sel), (value.len() as u16, info, value));
        }
    }
    impl XuTransport for FakeDevice {
        fn query(
            &self,
            unit: u8,
            selector: u8,
            query: Query,
            buf: &mut [u8],
        ) -> Result<(), namtor_uvc::probe::XuError> {
            let mut c = self.controls.borrow_mut();
            let Some((len, info, value)) = c.get_mut(&(unit, selector)) else {
                return Err(namtor_uvc::probe::XuError::Ioctl("ENOENT".into()));
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
                Query::SetCur => value.copy_from_slice(buf),
                _ => return Err(namtor_uvc::probe::XuError::Ioctl("unsupported".into())),
            }
            Ok(())
        }
    }

    #[test]
    fn watchdog_not_expired_before_max_on_ms() {
        let clock = MockClock::new(Instant::now());
        let mut wd = Watchdog::new(2000, &clock);
        wd.arm();
        clock.advance(Duration::from_millis(1000));
        assert!(!wd.check_expired());
    }

    #[test]
    fn watchdog_fires_after_max_on_ms() {
        let clock = MockClock::new(Instant::now());
        let mut wd = Watchdog::new(2000, &clock);
        wd.arm();
        clock.advance(Duration::from_millis(2001));
        assert!(wd.check_expired());
    }

    #[test]
    fn disarm_prevents_expiry() {
        let clock = MockClock::new(Instant::now());
        let mut wd = Watchdog::new(2000, &clock);
        wd.arm();
        clock.advance(Duration::from_millis(1000));
        wd.disarm();
        clock.advance(Duration::from_millis(5000));
        assert!(!wd.check_expired(), "disarmed watchdog must not fire");
    }

    #[test]
    fn arm_check_disarm_cycle() {
        let clock = MockClock::new(Instant::now());
        let mut wd = Watchdog::new(100, &clock);
        assert!(!wd.is_armed());
        wd.arm();
        assert!(wd.is_armed());
        clock.advance(Duration::from_millis(50));
        assert!(!wd.check_expired());
        wd.disarm();
        assert!(!wd.is_armed());
    }

    #[test]
    fn force_off_lib_writes_payload_off() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = EmitterCfg {
            method: namtor_hwdb::quirks::EmitterMethod::MsxuMode,
            guid: None,
            vc_interface: None,
            unit: Some(4),
            selector: Some(6),
            length: None,
            payload_on: vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            payload_off: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            mode: None,
            strobe: None,
            idle: None,
            lifecycle: None,
        };
        force_off_lib(&mut xu, &cfg).unwrap();
        let profile = xu.probe_control(4, 6).unwrap();
        assert_eq!(profile.current, vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn force_off_lib_is_best_effort_on_absent_control() {
        let dev = FakeDevice::new();
        let mut xu = SafeXu::new(dev);
        let cfg = EmitterCfg {
            method: namtor_hwdb::quirks::EmitterMethod::MsxuMode,
            guid: None,
            vc_interface: None,
            unit: Some(4),
            selector: Some(6),
            length: None,
            payload_on: vec![],
            payload_off: vec![],
            mode: None,
            strobe: None,
            idle: None,
            lifecycle: None,
        };
        assert!(
            force_off_lib(&mut xu, &cfg).is_ok(),
            "absent control must not error"
        );
    }

    #[test]
    fn enforce_off_if_expired_forces_off_and_disarms() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let clock = MockClock::new(Instant::now());
        let mut wd = Watchdog::new(2000, &clock);
        wd.arm();
        clock.advance(Duration::from_millis(2001));
        assert!(wd.is_armed());
        let cfg = EmitterCfg {
            method: namtor_hwdb::quirks::EmitterMethod::MsxuMode,
            guid: None,
            vc_interface: None,
            unit: Some(4),
            selector: Some(6),
            length: None,
            payload_on: vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            payload_off: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            mode: None,
            strobe: None,
            idle: None,
            lifecycle: None,
        };
        let forced = enforce_off_if_expired(&mut wd, &mut xu, &cfg).unwrap();
        assert!(forced, "expired watchdog must force off");
        assert!(!wd.is_armed(), "force-off must disarm the watchdog");
        let profile = xu.probe_control(4, 6).unwrap();
        assert_eq!(
            profile.current,
            vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            "payload_off must be written"
        );
    }
}
