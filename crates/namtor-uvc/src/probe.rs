// SPDX-License-Identifier: Apache-2.0
//! Safe extension unit access. Structurally enforces the probe safety rules
//! from spec §4.3 so a caller cannot bypass them.

use crate::xu::{CtrlInfo, Query};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum XuError {
    #[error("control {unit}/{selector:#04x} was never read; refusing to write")]
    NotRead { unit: u8, selector: u8 },
    #[error("control {unit}/{selector:#04x} does not support SET")]
    NoSetSupport { unit: u8, selector: u8 },
    #[error("payload length mismatch: control expects {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
    #[error("ioctl failed: {0}")]
    Ioctl(String),
}

/// Raw transport for extension unit queries. Implemented by the live device
/// and by test fakes.
pub trait XuTransport {
    fn query(&self, unit: u8, selector: u8, query: Query, buf: &mut [u8]) -> Result<(), XuError>;
}

/// What a probe learned about one control.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlProfile {
    pub len: usize,
    pub info: CtrlInfo,
    pub current: Vec<u8>,
}

/// Restores a control's prior value when dropped, unless defused with
/// [`Restorer::keep`]. Implements probe safety rule 2.
#[derive(Debug)]
pub struct Restorer<'a, T: XuTransport> {
    transport: &'a T,
    unit: u8,
    selector: u8,
    original: Vec<u8>,
    armed: bool,
}

impl<T: XuTransport> Restorer<'_, T> {
    /// Keeps the newly written value instead of restoring.
    pub fn keep(mut self) {
        self.armed = false;
    }
}

impl<T: XuTransport> Drop for Restorer<'_, T> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut buf = self.original.clone();
        // Best effort: a failure here is logged by the caller's journal, but
        // must not panic during unwind.
        let _ = self
            .transport
            .query(self.unit, self.selector, Query::SetCur, &mut buf);
    }
}

/// Extension unit accessor that structurally enforces the probe safety rules.
pub struct SafeXu<T: XuTransport> {
    transport: T,
    profiles: HashMap<(u8, u8), ControlProfile>,
}

impl<T: XuTransport> SafeXu<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            profiles: HashMap::new(),
        }
    }

    /// Reads GET_LEN, GET_INFO, and GET_CUR for a control, caching the result.
    ///
    /// A control must be probed before it can be written (rule 1).
    pub fn probe_control(&mut self, unit: u8, selector: u8) -> Result<ControlProfile, XuError> {
        let mut len_buf = [0u8; 2];
        self.transport
            .query(unit, selector, Query::GetLen, &mut len_buf)?;
        let len = u16::from_le_bytes(len_buf) as usize;

        let mut info_buf = [0u8; 1];
        self.transport
            .query(unit, selector, Query::GetInfo, &mut info_buf)?;
        let info = CtrlInfo::from_bits(info_buf[0]);

        let mut current = vec![0u8; len];
        self.transport
            .query(unit, selector, Query::GetCur, &mut current)?;

        let profile = ControlProfile { len, info, current };
        self.profiles.insert((unit, selector), profile.clone());
        Ok(profile)
    }

    /// Writes a control, returning a [`Restorer`] that puts the prior value
    /// back on drop.
    ///
    /// Refuses when the control was never probed (rule 1), does not support
    /// SET (rule 4), or the payload length disagrees with GET_LEN (rule 3).
    pub fn try_write(
        &mut self,
        unit: u8,
        selector: u8,
        payload: &[u8],
    ) -> Result<Restorer<'_, T>, XuError> {
        let profile = self
            .profiles
            .get(&(unit, selector))
            .ok_or(XuError::NotRead { unit, selector })?
            .clone();

        if !profile.info.supports_set() {
            return Err(XuError::NoSetSupport { unit, selector });
        }
        if payload.len() != profile.len {
            return Err(XuError::LengthMismatch {
                expected: profile.len,
                actual: payload.len(),
            });
        }

        let mut buf = payload.to_vec();
        self.transport
            .query(unit, selector, Query::SetCur, &mut buf)?;

        Ok(Restorer {
            transport: &self.transport,
            unit,
            selector,
            original: profile.current,
            armed: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// (unit, selector) -> (len, info byte, value)
    type ControlMap = HashMap<(u8, u8), (u16, u8, Vec<u8>)>;

    /// In-memory fake device. Records every query for assertion.
    #[derive(Debug)]
    struct FakeDevice {
        /// (unit, selector) -> (len, info byte, value)
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

    #[test]
    fn probe_reads_length_info_and_current_value() {
        let dev = FakeDevice::new();
        dev.add(3, 0x02, 0b11, vec![0xAA, 0xBB]);
        let mut xu = SafeXu::new(dev);

        let profile = xu.probe_control(3, 0x02).unwrap();

        assert_eq!(profile.len, 2);
        assert!(profile.info.supports_set());
        assert_eq!(profile.current, vec![0xAA, 0xBB]);
    }

    #[test]
    fn writing_without_probing_first_is_refused() {
        // Probe safety rule 1.
        let dev = FakeDevice::new();
        dev.add(3, 0x02, 0b11, vec![0x00]);
        let mut xu = SafeXu::new(dev);

        let err = xu.try_write(3, 0x02, &[0x01]).unwrap_err();
        assert!(matches!(err, XuError::NotRead { .. }));
    }

    #[test]
    fn writing_a_control_without_set_support_is_refused() {
        // Probe safety rule 4.
        let dev = FakeDevice::new();
        dev.add(3, 0x05, 0b01, vec![0x00]); // GET only
        let mut xu = SafeXu::new(dev);

        xu.probe_control(3, 0x05).unwrap();
        let err = xu.try_write(3, 0x05, &[0x01]).unwrap_err();
        assert!(matches!(err, XuError::NoSetSupport { .. }));
    }

    #[test]
    fn payload_length_must_match_get_len() {
        // Probe safety rule 3.
        let dev = FakeDevice::new();
        dev.add(3, 0x02, 0b11, vec![0x00, 0x00]);
        let mut xu = SafeXu::new(dev);

        xu.probe_control(3, 0x02).unwrap();
        let err = xu.try_write(3, 0x02, &[0x01]).unwrap_err();
        assert!(matches!(
            err,
            XuError::LengthMismatch {
                expected: 2,
                actual: 1
            }
        ));
    }

    #[test]
    fn dropping_the_restorer_puts_the_original_value_back() {
        // Probe safety rule 2.
        let dev = FakeDevice::new();
        dev.add(3, 0x02, 0b11, vec![0x00, 0x00]);
        let mut xu = SafeXu::new(dev);

        xu.probe_control(3, 0x02).unwrap();
        {
            let _r = xu.try_write(3, 0x02, &[0xFF, 0xFF]).unwrap();
            // Value is modified while the restorer is alive.
        }
        // Restorer dropped: the original value must have been written back.
        let profile = xu.probe_control(3, 0x02).unwrap();
        assert_eq!(profile.current, vec![0x00, 0x00]);
    }

    #[test]
    fn restorer_can_be_defused_when_a_setting_should_persist() {
        let dev = FakeDevice::new();
        dev.add(3, 0x02, 0b11, vec![0x00, 0x00]);
        let mut xu = SafeXu::new(dev);

        xu.probe_control(3, 0x02).unwrap();
        xu.try_write(3, 0x02, &[0xFF, 0xFF]).unwrap().keep();

        let profile = xu.probe_control(3, 0x02).unwrap();
        assert_eq!(profile.current, vec![0xFF, 0xFF]);
    }
}
