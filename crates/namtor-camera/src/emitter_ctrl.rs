// SPDX-License-Identifier: GPL-3.0-only
//! `EmitterController`: drives the IR emitter via a `SafeXu` transport,
//! wrapping the quirks entry's emitter configuration.
//!
//! Safety properties (spec §4.3, §4.10.2):
//!
//! - **Start-up reconciliation:** `force_off_before_anything()` is called
//!   inside `apply_on()` so a stale emitter state from a prior process or a
//!   crash never reaches authentication.
//! - **RAII off:** `Drop` forces the emitter off. A panic or early return
//!   cannot leave it on.
//! - **Verify:** after `apply_on()`, `verify()` round-trips GET_CUR to
//!   confirm the device accepted the payload.

use namtor_hwdb::quirks::{EmitterCfg, EmitterMethod};
use namtor_uvc::probe::{SafeXu, XuError, XuTransport};

#[derive(Debug, thiserror::Error)]
pub enum EmitterCtrlError {
    #[error("xu transport error: {0}")]
    Xu(#[from] XuError),
    #[error("emitter config missing required field: {0}")]
    MissingField(&'static str),
}

/// Consent gate for vendor extension-unit emitter probes. A vendor-XU write
/// is a blind write onto an unverified control, so it must not happen unless
/// the user has explicitly opted in (spec §4.10.1).
pub trait ConsentGate {
    fn allow_vendor_xu(&self) -> bool;
}

/// Reads consent from the `NAMTOR_VENDOR_XU_CONSENT` environment variable.
/// Returns `true` only when set to exactly `"1"`.
pub struct EnvConsent;

impl ConsentGate for EnvConsent {
    fn allow_vendor_xu(&self) -> bool {
        std::env::var("NAMTOR_VENDOR_XU_CONSENT").as_deref() == Ok("1")
    }
}

/// Outcome of a gated `apply_on`: which tier ran, and whether the emitter
/// was actually driven on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Activated,
    SkippedVendorXuNoConsent,
    SkippedAlwaysOn,
}

/// Owns the emitter control for one device. Generic over `XuTransport` so
/// tests inject a fake device.
pub struct EmitterController<'a, T: XuTransport> {
    xu: &'a mut SafeXu<T>,
    cfg: &'a EmitterCfg,
    /// The restorer from the most recent `apply_on`. Held so `off()` and
    /// `Drop` can restore the default value cleanly.
    armed: bool,
}

impl<'a, T: XuTransport> EmitterController<'a, T> {
    pub fn new(xu: &'a mut SafeXu<T>, cfg: &'a EmitterCfg) -> Self {
        Self {
            xu,
            cfg,
            armed: false,
        }
    }

    /// Forces the emitter off before any other operation. Called at the top
    /// of `apply_on()` so a stale state from a prior process is cleared.
    pub fn force_off_before_anything(&mut self) -> Result<(), EmitterCtrlError> {
        let unit = self
            .cfg
            .unit
            .ok_or(EmitterCtrlError::MissingField("unit"))?;
        let selector = self
            .cfg
            .selector
            .ok_or(EmitterCtrlError::MissingField("selector"))?;
        let payload = &self.cfg.payload_off;

        // Probe the control if not already profiled, then write the off
        // payload. Best-effort: if the control is absent, there is nothing
        // to force off.
        if self.xu.probe_control(unit, selector).is_err() {
            return Ok(());
        }
        match self.xu.try_write(unit, selector, payload) {
            Ok(restorer) => {
                restorer.keep();
                Ok(())
            }
            Err(_) => Ok(()),
        }
    }

    /// Turns the emitter on. Calls `force_off_before_anything()` first, then
    /// writes `payload_on` (for MsxuMode, this is the strobe-mode payload).
    pub fn apply_on(&mut self) -> Result<(), EmitterCtrlError> {
        self.force_off_before_anything()?;

        let unit = self
            .cfg
            .unit
            .ok_or(EmitterCtrlError::MissingField("unit"))?;
        let selector = self
            .cfg
            .selector
            .ok_or(EmitterCtrlError::MissingField("selector"))?;
        let payload = &self.cfg.payload_on;

        self.xu.probe_control(unit, selector)?;
        let restorer = self.xu.try_write(unit, selector, payload)?;
        restorer.keep();
        self.armed = true;
        Ok(())
    }

    /// Tiered, consent-gated emitter activation (spec §4.10.1-2). Always
    /// runs the startup force-off first. MSXU and firmware-strobed methods
    /// activate silently (no consent — they are documented controls).
    /// Vendor-XU activates only with consent. AlwaysOn is skipped (no write
    /// to a non-existent control wire).
    pub fn apply_on_gated(
        &mut self,
        consent: &dyn ConsentGate,
    ) -> Result<ApplyOutcome, EmitterCtrlError> {
        let _ = self.force_off_before_anything();
        match self.cfg.method {
            EmitterMethod::AlwaysOn => Ok(ApplyOutcome::SkippedAlwaysOn),
            EmitterMethod::VendorXu => {
                if consent.allow_vendor_xu() {
                    self.apply_on_no_force()?;
                    Ok(ApplyOutcome::Activated)
                } else {
                    Ok(ApplyOutcome::SkippedVendorXuNoConsent)
                }
            }
            EmitterMethod::MsxuMode
            | EmitterMethod::Msxu
            | EmitterMethod::FirmwareStrobed
            | EmitterMethod::Unknown => {
                self.apply_on_no_force()?;
                Ok(ApplyOutcome::Activated)
            }
        }
    }

    /// Writes `payload_on` without re-running the startup force-off (the
    /// gated entry point already did it).
    fn apply_on_no_force(&mut self) -> Result<(), EmitterCtrlError> {
        let unit = self
            .cfg
            .unit
            .ok_or(EmitterCtrlError::MissingField("unit"))?;
        let selector = self
            .cfg
            .selector
            .ok_or(EmitterCtrlError::MissingField("selector"))?;
        let payload = &self.cfg.payload_on;
        self.xu.probe_control(unit, selector)?;
        let restorer = self.xu.try_write(unit, selector, payload)?;
        restorer.keep();
        self.armed = true;
        Ok(())
    }

    /// Turns the emitter off by writing `payload_off`.
    pub fn off(&mut self) -> Result<(), EmitterCtrlError> {
        let unit = self
            .cfg
            .unit
            .ok_or(EmitterCtrlError::MissingField("unit"))?;
        let selector = self
            .cfg
            .selector
            .ok_or(EmitterCtrlError::MissingField("selector"))?;
        let payload = &self.cfg.payload_off;

        if self.xu.probe_control(unit, selector).is_err() {
            return Ok(());
        }
        match self.xu.try_write(unit, selector, payload) {
            Ok(restorer) => {
                restorer.keep();
                self.armed = false;
                Ok(())
            }
            Err(_) => Ok(()),
        }
    }

    /// Reads GET_CUR and returns the current payload bytes.
    pub fn verify(&mut self) -> Result<Vec<u8>, EmitterCtrlError> {
        let unit = self
            .cfg
            .unit
            .ok_or(EmitterCtrlError::MissingField("unit"))?;
        let selector = self
            .cfg
            .selector
            .ok_or(EmitterCtrlError::MissingField("selector"))?;
        let profile = self.xu.probe_control(unit, selector)?;
        Ok(profile.current)
    }

    /// The configured method, for the daemon's status reporting.
    pub fn method(&self) -> EmitterMethod {
        self.cfg.method
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Releases the armed state without writing `payload_off`. The caller
    /// takes responsibility for forcing the emitter off. Used by the daemon
    /// to transfer emitter ownership from the short-lived controller (which
    /// forces off on `Drop`) to the long-lived `EmitterGuard`.
    pub fn release_to_guard(&mut self) {
        self.armed = false;
    }
}

impl<T: XuTransport> Drop for EmitterController<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.off();
        }
    }
}

// Re-export Query so the bin helper in Task 16 can use it without reaching
// into namtor-uvc::xu directly.
pub use namtor_uvc::xu::Query as XuQuery;

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
        log: RefCell<Vec<(u8, u8, Query)>>,
    }

    impl FakeDevice {
        fn new() -> Self {
            Self {
                controls: RefCell::new(HashMap::new()),
                log: RefCell::new(Vec::new()),
            }
        }

        fn add(&self, unit: u8, sel: u8, info: u8, value: Vec<u8>) {
            self.controls
                .borrow_mut()
                .insert((unit, sel), (value.len() as u16, info, value));
        }

        #[allow(dead_code)]
        fn current(&self, unit: u8, sel: u8) -> Vec<u8> {
            self.controls
                .borrow()
                .get(&(unit, sel))
                .map(|(_, _, v)| v.clone())
                .unwrap_or_default()
        }

        #[allow(dead_code)]
        fn writes(&self) -> Vec<(u8, u8)> {
            self.log
                .borrow()
                .iter()
                .filter(|(_, _, q)| *q == Query::SetCur)
                .map(|(u, s, _)| (*u, *s))
                .collect()
        }
    }

    impl XuTransport for FakeDevice {
        fn query(
            &self,
            unit: u8,
            selector: u8,
            query: Query,
            buf: &mut [u8],
        ) -> Result<(), XuError> {
            self.log.borrow_mut().push((unit, selector, query));
            let mut controls = self.controls.borrow_mut();
            let Some((len, info, value)) = controls.get_mut(&(unit, selector)) else {
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
                Query::SetCur => value.copy_from_slice(buf),
                _ => return Err(XuError::Ioctl("unsupported".into())),
            }
            Ok(())
        }
    }

    fn msxu_cfg() -> EmitterCfg {
        EmitterCfg {
            method: EmitterMethod::MsxuMode,
            guid: Some("0f3f95dc-2632-4c4e-92c9-a04782f43bc8".into()),
            vc_interface: Some(2),
            unit: Some(4),
            selector: Some(6),
            length: Some(9),
            payload_on: vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            payload_off: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            mode: Some(2),
            strobe: None,
            idle: None,
            lifecycle: None,
        }
    }

    #[test]
    fn apply_on_writes_payload_on_and_drop_restores_off() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = msxu_cfg();

        {
            let mut ctrl = EmitterController::new(&mut xu, &cfg);
            ctrl.apply_on().unwrap();
            assert!(ctrl.is_armed());
            assert_eq!(
                ctrl.verify().unwrap(),
                vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
                "GET_CUR must read back payload_on"
            );
        }
        let profile = xu.probe_control(4, 6).unwrap();
        assert_eq!(
            profile.current,
            vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            "Drop must have written payload_off"
        );
    }

    #[test]
    fn force_off_before_anything_writes_payload_off_first() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = msxu_cfg();

        {
            let mut ctrl = EmitterController::new(&mut xu, &cfg);
            ctrl.force_off_before_anything().unwrap();
        }
        let profile = xu.probe_control(4, 6).unwrap();
        assert_eq!(
            profile.current,
            vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            "force_off must write payload_off"
        );
    }

    #[test]
    fn off_writes_payload_off_and_disarms() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = msxu_cfg();

        let mut ctrl = EmitterController::new(&mut xu, &cfg);
        ctrl.apply_on().unwrap();
        assert!(ctrl.is_armed());
        ctrl.off().unwrap();
        assert!(!ctrl.is_armed(), "off() must disarm");
    }

    #[test]
    fn verify_returns_current_bytes() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = msxu_cfg();

        let mut ctrl = EmitterController::new(&mut xu, &cfg);
        let cur = ctrl.verify().unwrap();
        assert_eq!(cur, vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn drop_does_not_off_when_not_armed() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = msxu_cfg();
        {
            let _ctrl = EmitterController::new(&mut xu, &cfg);
        }
        let profile = xu.probe_control(4, 6).unwrap();
        assert_eq!(profile.current, vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0]);
    }

    struct MockConsent {
        allow: bool,
    }
    impl ConsentGate for MockConsent {
        fn allow_vendor_xu(&self) -> bool {
            self.allow
        }
    }

    fn vendor_xu_cfg() -> EmitterCfg {
        EmitterCfg {
            method: EmitterMethod::VendorXu,
            guid: Some("11111111-2222-3333-4444-555555555555".into()),
            vc_interface: Some(2),
            unit: Some(4),
            selector: Some(9),
            length: Some(3),
            payload_on: vec![0x01, 0x00, 0x01],
            payload_off: vec![0x01, 0x00, 0x00],
            mode: None,
            strobe: None,
            idle: None,
            lifecycle: None,
        }
    }

    fn always_on_cfg() -> EmitterCfg {
        EmitterCfg {
            method: EmitterMethod::AlwaysOn,
            guid: None,
            vc_interface: None,
            unit: None,
            selector: None,
            length: None,
            payload_on: vec![],
            payload_off: vec![],
            mode: None,
            strobe: None,
            idle: None,
            lifecycle: None,
        }
    }

    #[test]
    fn apply_on_gated_msxu_silent_without_consent() {
        let dev = FakeDevice::new();
        dev.add(4, 6, 0b11, vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0]);
        let mut xu = SafeXu::new(dev);
        let cfg = msxu_cfg();
        let consent = MockConsent { allow: false };
        let mut ctrl = EmitterController::new(&mut xu, &cfg);
        let outcome = ctrl.apply_on_gated(&consent).unwrap();
        assert_eq!(outcome, ApplyOutcome::Activated);
        assert_eq!(
            ctrl.verify().unwrap(),
            vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0],
            "MSXU must activate silently even without consent"
        );
    }

    #[test]
    fn apply_on_gated_vendor_xu_without_consent_skips() {
        let dev = FakeDevice::new();
        dev.add(4, 9, 0b11, vec![0x01, 0x00, 0x00]);
        let mut xu = SafeXu::new(dev);
        let cfg = vendor_xu_cfg();
        let consent = MockConsent { allow: false };
        let mut ctrl = EmitterController::new(&mut xu, &cfg);
        let outcome = ctrl.apply_on_gated(&consent).unwrap();
        assert_eq!(outcome, ApplyOutcome::SkippedVendorXuNoConsent);
        assert!(
            !ctrl.is_armed(),
            "vendor XU without consent must not arm the emitter"
        );
        assert_eq!(
            ctrl.verify().unwrap(),
            vec![0x01, 0x00, 0x00],
            "payload_on must not be written without consent"
        );
    }

    #[test]
    fn apply_on_gated_vendor_xu_with_consent_activates() {
        let dev = FakeDevice::new();
        dev.add(4, 9, 0b11, vec![0x01, 0x00, 0x00]);
        let mut xu = SafeXu::new(dev);
        let cfg = vendor_xu_cfg();
        let consent = MockConsent { allow: true };
        let mut ctrl = EmitterController::new(&mut xu, &cfg);
        let outcome = ctrl.apply_on_gated(&consent).unwrap();
        assert_eq!(outcome, ApplyOutcome::Activated);
        assert!(ctrl.is_armed());
        assert_eq!(
            ctrl.verify().unwrap(),
            vec![0x01, 0x00, 0x01],
            "payload_on must be written with consent"
        );
    }

    #[test]
    fn apply_on_gated_always_on_skips_without_write() {
        let dev = FakeDevice::new();
        let mut xu = SafeXu::new(dev);
        let cfg = always_on_cfg();
        let consent = MockConsent { allow: true };
        let mut ctrl = EmitterController::new(&mut xu, &cfg);
        let outcome = ctrl.apply_on_gated(&consent).unwrap();
        assert_eq!(outcome, ApplyOutcome::SkippedAlwaysOn);
        assert!(!ctrl.is_armed());
    }
}
