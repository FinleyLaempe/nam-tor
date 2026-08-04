# Nam-tor M1 — Hardware Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retire risks R1, R2, and R3 — the only open unknowns that can invalidate the Nam-tor design — and produce quirks database entry #1 for the reference camera.

**Architecture:** A new `namtor-uvc` crate provides UVC extension unit access via `UVCIOC_CTRL_QUERY` ioctls, USB descriptor parsing from sysfs, and UVC payload-header metadata parsing. Everything that can be tested without hardware is: descriptor parsing against a committed fixture, metadata parsing against synthetic buffers, torn-frame classification against synthetic timings, and emitter verification against M0's synthetic frames. Only live ioctl execution requires the camera, and those tests are gated behind an environment variable so CI stays green.

**Tech Stack:** Rust (nix for ioctls, serde, thiserror), `namtor-capture` from M0, the reference Sonix `3277:0018` camera.

## Global Constraints

Copied from `docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md`. Every task's requirements implicitly include these.

- Rust edition 2021 or later. SPDX header on every source file.
- `namtor-uvc` is **Apache-2.0** — it is consumed by `namtor-hwdb` and `namtor-probe`, which the spec licenses permissively so any project can use the hardware database.
- TDD: test first, watch it fail, minimal implementation, watch it pass, commit.
- Commit after every task.
- Reference hardware: Arch Linux, kernel 7.1.x, Sonix `3277:0018` on USB `3-8`, IR node `/dev/video2`, IR metadata node `/dev/video3`, RGB node `/dev/video0`.

### Probe safety rules — spec §4.3, enforced in code

Blind writes to vendor extension units can hang a camera or reach firmware. These are not guidelines; Task 3 makes them structurally unavoidable:

1. Never write a control that has not been successfully `GET_CUR`-read.
2. Always round-trip: read current → modify → write → restore on failure or timeout.
3. Honour `GET_LEN` and `GET_INFO`; never guess payload length.
4. Skip controls whose `GET_INFO` does not report SET support.
5. Probing requires explicit user confirmation with the risk stated plainly.
6. Journal every probe action so a failed probe is reconstructable.

### Hardware-gated tests

Tests requiring the real camera are marked `#[ignore]` and run with:

```bash
NAMTOR_HW_TEST=1 cargo test -p namtor-uvc -- --ignored --test-threads=1
```

CI never sets `NAMTOR_HW_TEST`. Hardware tests are run by the developer on the reference machine and their output pasted into the spike report (Task 15).

---

### Task 1: XU query types and ioctl binding

**Files:**
- Create: `crates/namtor-uvc/Cargo.toml`
- Create: `crates/namtor-uvc/src/lib.rs`
- Create: `crates/namtor-uvc/src/xu.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: nothing
- Produces: `UvcXuControlQuery` repr(C) struct matching the kernel's `struct uvc_xu_control_query`; `Query` enum with discriminants `SetCur = 0x01`, `GetCur = 0x81`, `GetMin = 0x82`, `GetMax = 0x83`, `GetRes = 0x84`, `GetLen = 0x85`, `GetInfo = 0x86`, `GetDef = 0x87`; the `uvc_ctrl_query` ioctl binding.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/xu.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! UVC extension unit control queries.

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn query_struct_matches_kernel_abi() {
        // struct uvc_xu_control_query {
        //     __u8 unit; __u8 selector; __u8 query;
        //     __u16 size; __u8 __user *data;
        // };
        // On LP64: u8 u8 u8 pad, u16 at 4, pad, pointer at 8. Total 16.
        assert_eq!(size_of::<UvcXuControlQuery>(), 16);
        assert_eq!(align_of::<UvcXuControlQuery>(), 8);
    }

    #[test]
    fn query_codes_match_linux_usb_video_h() {
        assert_eq!(Query::SetCur as u8, 0x01);
        assert_eq!(Query::GetCur as u8, 0x81);
        assert_eq!(Query::GetMin as u8, 0x82);
        assert_eq!(Query::GetMax as u8, 0x83);
        assert_eq!(Query::GetRes as u8, 0x84);
        assert_eq!(Query::GetLen as u8, 0x85);
        assert_eq!(Query::GetInfo as u8, 0x86);
        assert_eq!(Query::GetDef as u8, 0x87);
    }

    #[test]
    fn set_cur_is_the_only_write_query() {
        assert!(Query::SetCur.is_write());
        for q in [
            Query::GetCur, Query::GetMin, Query::GetMax,
            Query::GetRes, Query::GetLen, Query::GetInfo, Query::GetDef,
        ] {
            assert!(!q.is_write(), "{q:?} must not be classified as a write");
        }
    }
}
```

Create `crates/namtor-uvc/src/lib.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! UVC extension unit access, USB descriptor parsing, and payload-header
//! metadata parsing for Nam-tor.
//!
//! Licensed Apache-2.0 so any project can consume the hardware database this
//! crate underpins. IR camera detection is a Linux-wide problem.

pub mod xu;
```

Create `crates/namtor-uvc/Cargo.toml`:

```toml
[package]
name = "namtor-uvc"
version = "0.1.0"
edition.workspace = true
license = "Apache-2.0"

[dependencies]
nix = { version = "0.29", features = ["ioctl"] }
serde = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

Add to the workspace `Cargo.toml` members list:

```toml
members = [
    "crates/namtor-capture",
    "crates/namtor-eval",
    "crates/namtor-uvc",
]
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc xu::`
Expected: FAIL — `cannot find struct UvcXuControlQuery in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/xu.rs` above the `tests` module:

```rust
use std::os::raw::c_void;

/// Mirrors the kernel's `struct uvc_xu_control_query` from
/// `include/uapi/linux/uvcvideo.h`.
#[repr(C)]
#[derive(Debug)]
pub struct UvcXuControlQuery {
    pub unit: u8,
    pub selector: u8,
    pub query: u8,
    pub size: u16,
    pub data: *mut c_void,
}

/// Video Class-Specific Request Codes, `include/uapi/linux/usb/video.h` A.8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Query {
    SetCur = 0x01,
    GetCur = 0x81,
    GetMin = 0x82,
    GetMax = 0x83,
    GetRes = 0x84,
    GetLen = 0x85,
    GetInfo = 0x86,
    GetDef = 0x87,
}

impl Query {
    /// True for queries that modify device state.
    ///
    /// Used by the safe wrapper to enforce the probe safety rules: a write is
    /// only permitted after a successful read of the same control.
    pub fn is_write(self) -> bool {
        matches!(self, Query::SetCur)
    }
}

// _IOWR('u', 0x21, struct uvc_xu_control_query)
nix::ioctl_readwrite!(uvc_ctrl_query, b'u', 0x21, UvcXuControlQuery);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc xu::`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc Cargo.toml
git commit -m "feat(uvc): extension unit query types and ioctl binding"
```

---

### Task 2: GET_INFO capability flags

**Files:**
- Modify: `crates/namtor-uvc/src/xu.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `CtrlInfo` bitflag wrapper with `from_bits(u8)`, `supports_get()`, `supports_set()`, `is_disabled()`, `is_autoupdate()`, `is_async()`.

**Why this matters:** rule 4 of the probe safety rules requires skipping controls that do not report SET support. That check needs a correct decoding of the GET_INFO byte.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/namtor-uvc/src/xu.rs`:

```rust
    #[test]
    fn ctrl_info_decodes_get_and_set_support() {
        // UVC 1.5 §4.1.2: bit 0 = GET supported, bit 1 = SET supported.
        let both = CtrlInfo::from_bits(0b0000_0011);
        assert!(both.supports_get());
        assert!(both.supports_set());

        let read_only = CtrlInfo::from_bits(0b0000_0001);
        assert!(read_only.supports_get());
        assert!(!read_only.supports_set());
    }

    #[test]
    fn ctrl_info_decodes_state_bits() {
        // bit 2 = disabled due to automatic mode, bit 3 = autoupdate,
        // bit 4 = asynchronous.
        assert!(CtrlInfo::from_bits(0b0000_0100).is_disabled());
        assert!(CtrlInfo::from_bits(0b0000_1000).is_autoupdate());
        assert!(CtrlInfo::from_bits(0b0001_0000).is_async());
    }

    #[test]
    fn a_control_with_no_set_support_is_never_probe_writable() {
        // Probe safety rule 4.
        let info = CtrlInfo::from_bits(0b0000_0001);
        assert!(!info.supports_set());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc xu::`
Expected: FAIL — `cannot find struct CtrlInfo in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/xu.rs`:

```rust
/// Decoded GET_INFO capability byte, UVC 1.5 §4.1.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlInfo(u8);

impl CtrlInfo {
    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn supports_get(self) -> bool {
        self.0 & 0b0000_0001 != 0
    }

    /// Probe safety rule 4: controls without SET support are never written.
    pub fn supports_set(self) -> bool {
        self.0 & 0b0000_0010 != 0
    }

    pub fn is_disabled(self) -> bool {
        self.0 & 0b0000_0100 != 0
    }

    pub fn is_autoupdate(self) -> bool {
        self.0 & 0b0000_1000 != 0
    }

    pub fn is_async(self) -> bool {
        self.0 & 0b0001_0000 != 0
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc xu::`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/xu.rs
git commit -m "feat(uvc): GET_INFO capability flag decoding"
```

---

### Task 3: Safe XU accessor enforcing the probe rules

**Files:**
- Create: `crates/namtor-uvc/src/probe.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: `Query`, `CtrlInfo` from Tasks 1–2
- Produces: `trait XuTransport { fn query(&self, unit: u8, selector: u8, query: Query, buf: &mut [u8]) -> Result<(), XuError>; }`; `SafeXu<T: XuTransport>` with `probe_control(&mut self, unit, selector) -> Result<ControlProfile, XuError>` and `try_write(&mut self, unit, selector, payload: &[u8]) -> Result<Restorer, XuError>`; `ControlProfile { len, info, current }`; `Restorer` restoring the prior value on `Drop`; `XuError` enum including `NotRead`, `NoSetSupport`, `LengthMismatch`, `Ioctl`.

**Design:** `try_write` is only reachable after `probe_control` has cached a `ControlProfile` for that control, so rules 1–4 cannot be bypassed by a caller. `Restorer` implements rule 2's restore-on-failure via RAII.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/probe.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Safe extension unit access. Structurally enforces the probe safety rules
//! from spec §4.3 so a caller cannot bypass them.

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// In-memory fake device. Records every query for assertion.
    struct FakeDevice {
        /// (unit, selector) -> (len, info byte, value)
        controls: RefCell<HashMap<(u8, u8), (u16, u8, Vec<u8>)>>,
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
            XuError::LengthMismatch { expected: 2, actual: 1 }
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
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod probe;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc probe::`
Expected: FAIL — `cannot find trait XuTransport in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/probe.rs` above the `tests` module:

```rust
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
    fn query(
        &self,
        unit: u8,
        selector: u8,
        query: Query,
        buf: &mut [u8],
    ) -> Result<(), XuError>;
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
    pub fn probe_control(
        &mut self,
        unit: u8,
        selector: u8,
    ) -> Result<ControlProfile, XuError> {
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

        let profile = ControlProfile {
            len,
            info,
            current,
        };
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc probe::`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/probe.rs crates/namtor-uvc/src/lib.rs
git commit -m "feat(uvc): safe XU accessor enforcing probe safety rules"
```

---

### Task 4: USB descriptor parsing from sysfs

**Files:**
- Create: `crates/namtor-uvc/src/descriptors.rs`
- Create: `fixtures/descriptors/3277-0018.bin`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `ExtensionUnit { interface, unit_id, guid: [u8; 16], bm_controls: Vec<u8> }` with `advertised_selectors(&self) -> Vec<u8>`; `FormatDescriptor { interface, format_index, guid: [u8; 16] }`; `parse_descriptors(blob: &[u8]) -> DescriptorSet`; `DescriptorSet { extension_units, formats }`; `guid_leading_dword(guid) -> u32`; `is_l8_ir_format(guid) -> bool`; `format_guid(&[u8; 16]) -> String`.

**Why this task exists:** spec §4.1 claims the IR sensor is identifiable by its `MFVideoFormat_L8` format GUID, which `uvcvideo` discards before userspace sees it. This task proves that claim against the real device's descriptor blob.

**These assertions were validated against the real blob on 2026-08-04** and encode a correction that cost real time to find. The device has **two VideoControl interfaces, one per sensor**, each declaring both extension units:

```
if0 unit=3  28f03370-…  bmControls=bf      colour sensor, vendor XU
if0 unit=4  0f3f95dc-…  bmControls=0000    colour sensor, MSXU
if2 unit=3  28f03370-…  bmControls=00      IR sensor, vendor XU — nothing advertised
if2 unit=4  0f3f95dc-…  bmControls=2001    IR sensor, MSXU — selectors 6 and 9
```

A truncated `lsusb` reading attributes the colour sensor's `bf` to the IR sensor and sends emitter probing at the wrong device. **Always filter extension units by `interface` before probing.**

- [ ] **Step 1: Verify the fixture**

**The fixture is already committed.** It was captured and validated on 2026-08-04, and the expected values below were derived from this exact blob. Verify you have it:

```bash
sha256sum fixtures/descriptors/3277-0018.bin
```

Expected: `68936ace0b0d04288d5a5d51342671fc7d488a80b42dd0d11e22a1fdf7e8caa7`, 1119 bytes.

If the file is missing or the hash differs, recapture and expect the assertions in this task to need updating for your device:

```bash
mkdir -p fixtures/descriptors
cp /sys/bus/usb/devices/3-8/descriptors fixtures/descriptors/3277-0018.bin
```

If `3-8` is not the camera, find it with `ls -l /sys/bus/usb/devices/*/idVendor` and match `3277`.

- [ ] **Step 2: Write the failing test**

Create `crates/namtor-uvc/src/descriptors.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! USB descriptor parsing.
//!
//! `uvcvideo` collapses the IR sensor's format GUID to V4L2 `GREY` and
//! discards the GUID. `GREY` means greyscale, not infrared — which is why
//! every other Linux tool guesses. Reading the raw descriptors from sysfs
//! recovers the same signal Windows Hello keys on.

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_blob() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/descriptors/3277-0018.bin"
        ))
        .expect("reference descriptor fixture missing; see Task 4 Step 1")
    }

    #[test]
    fn parses_four_extension_units_two_per_videocontrol_interface() {
        // The device has TWO VideoControl interfaces, one per sensor, and each
        // declares both extension units. Reading this as "two units" attributes
        // the colour sensor's control bitmap to the IR sensor and sends emitter
        // probing at the wrong device.
        let set = parse_descriptors(&reference_blob());
        assert_eq!(set.extension_units.len(), 4);
    }

    #[test]
    fn the_ir_interfaces_vendor_xu_advertises_no_controls() {
        let set = parse_descriptors(&reference_blob());

        let sonix_ir = set
            .extension_units
            .iter()
            .find(|xu| {
                xu.interface == 2 && format_guid(&xu.guid).starts_with("28f03370-6311-4a2e")
            })
            .expect("Sonix vendor XU on the IR interface not found");

        assert_eq!(sonix_ir.bm_controls, vec![0x00]);
    }

    #[test]
    fn the_colour_interfaces_vendor_xu_is_the_one_advertising_bf() {
        // Guards against re-attributing this bitmap to the IR sensor.
        let set = parse_descriptors(&reference_blob());

        let sonix_rgb = set
            .extension_units
            .iter()
            .find(|xu| {
                xu.interface == 0 && format_guid(&xu.guid).starts_with("28f03370-6311-4a2e")
            })
            .expect("Sonix vendor XU on the colour interface not found");

        assert_eq!(sonix_rgb.bm_controls, vec![0xbf]);
    }

    #[test]
    fn the_ir_interfaces_msxu_advertises_selectors_six_and_nine() {
        // The primary emitter candidates on this device.
        let set = parse_descriptors(&reference_blob());

        let msxu_ir = set
            .extension_units
            .iter()
            .find(|xu| {
                xu.interface == 2 && format_guid(&xu.guid).starts_with("0f3f95dc-2632-4c4e")
            })
            .expect("MSXU on the IR interface not found");

        assert_eq!(msxu_ir.unit_id, 4);
        assert_eq!(msxu_ir.bm_controls, vec![0x20, 0x01]);
        assert_eq!(msxu_ir.advertised_selectors(), vec![6, 9]);
    }

    #[test]
    fn the_colour_interfaces_msxu_advertises_nothing() {
        let set = parse_descriptors(&reference_blob());

        let msxu_rgb = set
            .extension_units
            .iter()
            .find(|xu| {
                xu.interface == 0 && format_guid(&xu.guid).starts_with("0f3f95dc-2632-4c4e")
            })
            .expect("MSXU on the colour interface not found");

        assert!(msxu_rgb.advertised_selectors().is_empty());
    }

    #[test]
    fn selector_mapping_is_one_based() {
        // UVC: bmControls bit n indicates the control with selector n+1.
        let xu = ExtensionUnit {
            interface: 0,
            unit_id: 1,
            guid: [0u8; 16],
            bm_controls: vec![0b0000_0001, 0b0000_0010],
        };
        // bit 0 -> selector 1; bit 9 -> selector 10.
        assert_eq!(xu.advertised_selectors(), vec![1, 10]);
    }

    #[test]
    fn identifies_the_ir_format_by_its_l8_guid() {
        // Spec §4.1: leading dword 0x32 == D3DFMT_L8 == MFVideoFormat_L8.
        // This is Hello's detection signal, recovered from the descriptors.
        let set = parse_descriptors(&reference_blob());

        let ir = set
            .formats
            .iter()
            .find(|f| is_l8_ir_format(&f.guid))
            .expect("no L8 format found; the IR detection premise is wrong");

        assert_eq!(guid_leading_dword(&ir.guid), 0x32);
    }

    #[test]
    fn the_colour_format_is_not_classified_as_ir() {
        let set = parse_descriptors(&reference_blob());

        // YUY2: leading dword 0x32595559 (ASCII "YUY2" little-endian).
        let yuy2 = set
            .formats
            .iter()
            .find(|f| guid_leading_dword(&f.guid) == 0x3259_5559)
            .expect("YUY2 format not found");

        assert!(!is_l8_ir_format(&yuy2.guid));
    }

    #[test]
    fn truncated_blobs_do_not_panic() {
        let blob = reference_blob();
        for cut in [0, 1, 5, 17, 40, blob.len() / 2] {
            let _ = parse_descriptors(&blob[..cut.min(blob.len())]);
        }
    }

    #[test]
    fn zero_length_descriptor_does_not_loop_forever() {
        // A malformed device reporting bLength 0 must not hang the parser.
        let blob = vec![0u8, 0x24, 0x06, 0x01];
        let set = parse_descriptors(&blob);
        assert!(set.extension_units.is_empty());
    }
}
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod descriptors;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p namtor-uvc descriptors::`
Expected: FAIL — `cannot find function parse_descriptors in this scope`.

- [ ] **Step 4: Write minimal implementation**

Add to `crates/namtor-uvc/src/descriptors.rs` above the `tests` module:

```rust
/// USB descriptor type for class-specific interface descriptors.
const CS_INTERFACE: u8 = 0x24;
/// Standard interface descriptor.
const DT_INTERFACE: u8 = 0x04;
/// VideoControl EXTENSION_UNIT subtype.
const VC_EXTENSION_UNIT: u8 = 0x06;
/// VideoStreaming FORMAT_UNCOMPRESSED subtype.
const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;

/// A UVC extension unit declared by the device.
///
/// Note that a multi-sensor camera declares **one set per VideoControl
/// interface**. Always filter by `interface` before probing, or emitter
/// discovery will poke the wrong sensor's controls.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionUnit {
    pub interface: u8,
    pub unit_id: u8,
    pub guid: [u8; 16],
    pub bm_controls: Vec<u8>,
}

impl ExtensionUnit {
    /// Control selectors this unit claims to support.
    ///
    /// UVC encodes `bmControls` as a little-endian bitmap in which bit *n*
    /// indicates the control with selector *n+1*. An empty result does not
    /// prove the unit is inert — vendors under-declare routinely — but a
    /// non-empty result is a strong starting point for probing.
    pub fn advertised_selectors(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (byte_index, byte) in self.bm_controls.iter().enumerate() {
            for bit in 0..8u32 {
                if byte & (1 << bit) != 0 {
                    out.push((byte_index as u32 * 8 + bit + 1) as u8);
                }
            }
        }
        out
    }
}

/// An uncompressed format declared by a streaming interface.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatDescriptor {
    pub interface: u8,
    pub format_index: u8,
    pub guid: [u8; 16],
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct DescriptorSet {
    pub extension_units: Vec<ExtensionUnit>,
    pub formats: Vec<FormatDescriptor>,
}

/// The leading dword of a Microsoft-style media format GUID.
///
/// Microsoft encodes the format identifier in the first four bytes,
/// little-endian, of a GUID otherwise following the
/// `XXXXXXXX-0000-0010-8000-00AA00389B71` template.
pub fn guid_leading_dword(guid: &[u8; 16]) -> u32 {
    u32::from_le_bytes([guid[0], guid[1], guid[2], guid[3]])
}

/// True when a format GUID identifies 8-bit luminance, i.e. an IR sensor.
///
/// `D3DFMT_L8` is 50 (`0x32`), so `MFVideoFormat_L8` carries `0x32` in its
/// leading dword. Classification keys on the leading dword alone: the
/// reference camera deviates from Microsoft's template in the second group
/// (`-0002-` rather than `-0000-`), and other vendors deviate elsewhere.
pub fn is_l8_ir_format(guid: &[u8; 16]) -> bool {
    guid_leading_dword(guid) == 0x32
}

/// Formats a GUID in the conventional `8-4-4-4-12` textual form, matching
/// what `lsusb -v` prints, so fixtures and logs can be compared by eye.
pub fn format_guid(g: &[u8; 16]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10..16].iter().map(|b| format!("{b:02x}")).collect::<String>(),
    )
}

/// Walks a raw USB descriptor blob, extracting extension units and formats.
///
/// Descriptors are a flat TLV sequence: `[bLength, bDescriptorType, ...]`.
/// Malformed input is skipped rather than fatal — a device that lies about
/// its own descriptors must not crash detection.
pub fn parse_descriptors(blob: &[u8]) -> DescriptorSet {
    let mut set = DescriptorSet::default();
    let mut current_interface: u8 = 0;
    let mut i: usize = 0;

    while i + 2 <= blob.len() {
        let len = blob[i] as usize;
        // A zero or undersized length would loop forever or read backwards.
        if len < 2 || i + len > blob.len() {
            break;
        }
        let desc = &blob[i..i + len];
        let dtype = desc[1];

        if dtype == DT_INTERFACE && len >= 3 {
            current_interface = desc[2];
        } else if dtype == CS_INTERFACE && len >= 3 {
            match desc[2] {
                VC_EXTENSION_UNIT => {
                    // bLength, bDescriptorType, bDescriptorSubtype, bUnitID,
                    // guidExtensionCode[16], bNumControls, bNrInPins,
                    // baSourceID[bNrInPins], bControlSize, bmControls[..],
                    // iExtension
                    if len >= 22 {
                        let mut guid = [0u8; 16];
                        guid.copy_from_slice(&desc[4..20]);
                        let nr_in_pins = desc[21] as usize;
                        let ctrl_size_at = 22 + nr_in_pins;

                        let bm_controls = if ctrl_size_at < len {
                            let ctrl_size = desc[ctrl_size_at] as usize;
                            let start = ctrl_size_at + 1;
                            let end = (start + ctrl_size).min(len);
                            desc[start..end].to_vec()
                        } else {
                            Vec::new()
                        };

                        set.extension_units.push(ExtensionUnit {
                            interface: current_interface,
                            unit_id: desc[3],
                            guid,
                            bm_controls,
                        });
                    }
                }
                VS_FORMAT_UNCOMPRESSED => {
                    // bLength, bDescriptorType, bDescriptorSubtype,
                    // bFormatIndex, bNumFrameDescriptors, guidFormat[16], ...
                    if len >= 21 {
                        let mut guid = [0u8; 16];
                        guid.copy_from_slice(&desc[5..21]);
                        set.formats.push(FormatDescriptor {
                            interface: current_interface,
                            format_index: desc[3],
                            guid,
                        });
                    }
                }
                _ => {}
            }
        }

        i += len;
    }

    set
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p namtor-uvc descriptors::`
Expected: PASS — 10 tests.

Troubleshooting, in order of likelihood:

- **Wrong `bm_controls` values:** the `bNrInPins` offset arithmetic is the usual culprit. `baSourceID` is `bNrInPins` bytes long and sits between `bNrInPins` and `bControlSize`.
- **Only two extension units parsed:** the interface tracking is not resetting on `DT_INTERFACE` (0x04) descriptors, so units from the second VideoControl interface are being merged or skipped.
- **Assertions differ from the values above:** the reference blob may have been captured from a different device path. Confirm with `ls -l /sys/bus/usb/devices/*/idVendor` and match `3277`.

A quick independent check, which is how these expected values were produced:

```bash
python3 -c "
import pathlib
b=pathlib.Path('fixtures/descriptors/3277-0018.bin').read_bytes()
i=0; iface=0
while i+2<=len(b):
    n=b[i]
    if n<2 or i+n>len(b): break
    d=b[i:i+n]
    if d[1]==0x04 and n>=3: iface=d[2]
    elif d[1]==0x24 and n>=22 and d[2]==0x06:
        nr=d[21]; cs=d[22+nr]
        print('if%d unit=%d bm=%s' % (iface, d[3], d[23+nr:23+nr+cs].hex()))
    i+=n"
```

- [ ] **Step 6: Commit**

```bash
git add crates/namtor-uvc/src/descriptors.rs crates/namtor-uvc/src/lib.rs \
        fixtures/descriptors/3277-0018.bin
git commit -m "feat(uvc): USB descriptor parsing, recovering the L8 IR signal"
```

---

### Task 5: UVC payload-header metadata parsing

**Files:**
- Create: `crates/namtor-uvc/src/metadata.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `UvcMetaBuf { ns: u64, sof: u16, length: u8, flags: u8, payload: Vec<u8> }`; `parse_meta(buf: &[u8]) -> Result<UvcMetaBuf, MetaError>`; `UvcMetaBuf::pts() -> Option<u32>`; `UvcMetaBuf::has_error() -> bool`; `UvcMetaBuf::end_of_frame() -> bool`.

**Format:** `struct uvc_meta_buf { __u64 ns; __u16 sof; __u8 length; __u8 flags; __u8 buf[]; } __packed;` — the `V4L2_META_FMT_UVC` buffer layout. `flags` is the UVC payload header's `bmHeaderInfo`: bit 1 end-of-frame, bit 2 PTS present, bit 3 SCR present, bit 6 error.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/metadata.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! UVC payload header metadata, from the `V4L2_META_FMT_UVC` node.
//!
//! This is what makes frame-accurate strobe pairing possible: extension unit
//! writes are USB control transfers with no frame synchronisation, so a
//! toggle can land mid-exposure. SOF and PTS let torn frames be identified
//! and discarded rather than silently corrupting the differential.

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
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod metadata;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc metadata::`
Expected: FAIL — `cannot find function parse_meta in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/metadata.rs` above the `tests` module:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc metadata::`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/metadata.rs crates/namtor-uvc/src/lib.rs
git commit -m "feat(uvc): payload header metadata parsing"
```

---

### Task 6: Torn-frame classification

**Files:**
- Create: `crates/namtor-uvc/src/strobe.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: `EmitterState` from `namtor-capture`
- Produces: `Toggle { at_ns: u64, to_on: bool }`; `FrameWindow { start_ns: u64, end_ns: u64 }`; `classify(window: FrameWindow, toggles: &[Toggle], initial: bool) -> EmitterState`; `StrobeSequencer` producing `FrameWindow` from consecutive metadata timestamps.

**Model:** a frame's metadata timestamp marks when its payload was received, i.e. end of frame. The window is `[end_ns - frame_duration_ns, end_ns]`. If any toggle falls strictly inside the window, the frame is `Torn`. Otherwise the state is whatever the most recent preceding toggle set.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/strobe.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Strobe sequencing and torn-frame rejection.
//!
//! Extension unit writes are USB control transfers with ~1–5 ms latency and
//! no frame synchronisation. At 30 fps a toggle can land mid-exposure,
//! producing a partially-lit frame. Such frames are discarded, never
//! averaged into a differential.

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::EmitterState;

    const MS: u64 = 1_000_000;

    fn window(start_ms: u64, end_ms: u64) -> FrameWindow {
        FrameWindow {
            start_ns: start_ms * MS,
            end_ns: end_ms * MS,
        }
    }

    #[test]
    fn frame_entirely_after_an_on_toggle_is_on() {
        let toggles = [Toggle { at_ns: 10 * MS, to_on: true }];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::On
        );
    }

    #[test]
    fn frame_entirely_before_any_toggle_keeps_the_initial_state() {
        let toggles = [Toggle { at_ns: 100 * MS, to_on: true }];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::Off
        );
    }

    #[test]
    fn toggle_inside_the_exposure_makes_the_frame_torn() {
        // The whole point of this module.
        let toggles = [Toggle { at_ns: 35 * MS, to_on: true }];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::Torn
        );
    }

    #[test]
    fn toggle_exactly_at_the_boundary_is_not_torn() {
        // A toggle at the instant the frame ends affects the next frame.
        let toggles = [Toggle { at_ns: 53 * MS, to_on: true }];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::Off
        );
    }

    #[test]
    fn multiple_toggles_inside_one_exposure_are_still_torn() {
        let toggles = [
            Toggle { at_ns: 30 * MS, to_on: true },
            Toggle { at_ns: 40 * MS, to_on: false },
        ];
        assert_eq!(
            classify(window(20, 53), &toggles, false),
            EmitterState::Torn
        );
    }

    #[test]
    fn sequencer_derives_windows_from_consecutive_timestamps() {
        let mut seq = StrobeSequencer::new(33_333_333);

        // First frame has no predecessor: the nominal duration is assumed.
        let w0 = seq.push(100 * MS);
        assert_eq!(w0.end_ns, 100 * MS);
        assert_eq!(w0.start_ns, 100 * MS - 33_333_333);

        // Second frame's window starts where the previous one ended.
        let w1 = seq.push(133 * MS);
        assert_eq!(w1.start_ns, 100 * MS);
        assert_eq!(w1.end_ns, 133 * MS);
    }

    #[test]
    fn sequencer_falls_back_to_nominal_duration_on_a_backwards_timestamp() {
        // Timestamps must be monotonic; a device that violates this must not
        // produce a window with start > end.
        let mut seq = StrobeSequencer::new(33_333_333);
        seq.push(200 * MS);
        let w = seq.push(100 * MS);
        assert!(w.start_ns <= w.end_ns);
    }
}
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod strobe;
```

Add to `crates/namtor-uvc/Cargo.toml` under `[dependencies]`:

```toml
namtor-capture = { path = "../namtor-capture" }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc strobe::`
Expected: FAIL — `cannot find function classify in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/strobe.rs` above the `tests` module:

```rust
use namtor_capture::frame::EmitterState;

/// A recorded emitter state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toggle {
    /// Monotonic nanoseconds, on the same clock as frame metadata.
    pub at_ns: u64,
    pub to_on: bool,
}

/// The exposure interval a frame covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameWindow {
    pub start_ns: u64,
    pub end_ns: u64,
}

/// Determines a frame's emitter state, or [`EmitterState::Torn`] when the
/// emitter changed during the exposure.
///
/// Boundary toggles are attributed to the *following* frame: a toggle at
/// exactly `end_ns` did not affect this exposure.
pub fn classify(
    window: FrameWindow,
    toggles: &[Toggle],
    initial_on: bool,
) -> EmitterState {
    let torn = toggles
        .iter()
        .any(|t| t.at_ns > window.start_ns && t.at_ns < window.end_ns);
    if torn {
        return EmitterState::Torn;
    }

    let state = toggles
        .iter()
        .filter(|t| t.at_ns <= window.start_ns)
        .max_by_key(|t| t.at_ns)
        .map(|t| t.to_on)
        .unwrap_or(initial_on);

    if state {
        EmitterState::On
    } else {
        EmitterState::Off
    }
}

/// Turns a stream of frame-arrival timestamps into exposure windows.
pub struct StrobeSequencer {
    nominal_duration_ns: u64,
    previous_end_ns: Option<u64>,
}

impl StrobeSequencer {
    /// `nominal_duration_ns` is `1e9 / fps`, used for the first frame and as
    /// a fallback when timestamps are not monotonic.
    pub fn new(nominal_duration_ns: u64) -> Self {
        Self {
            nominal_duration_ns,
            previous_end_ns: None,
        }
    }

    /// Records a frame arrival and returns its exposure window.
    pub fn push(&mut self, end_ns: u64) -> FrameWindow {
        let start_ns = match self.previous_end_ns {
            Some(prev) if prev <= end_ns => prev,
            _ => end_ns.saturating_sub(self.nominal_duration_ns),
        };
        self.previous_end_ns = Some(end_ns);
        FrameWindow { start_ns, end_ns }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc strobe::`
Expected: PASS — 7 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/strobe.rs crates/namtor-uvc/src/lib.rs \
        crates/namtor-uvc/Cargo.toml
git commit -m "feat(uvc): strobe sequencing and torn-frame classification"
```

---

### Task 7: Automatic emitter verification by luminance delta

**Files:**
- Create: `crates/namtor-uvc/src/emitter.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: `Frame` from `namtor-capture`
- Produces: `LumaStats { mean: f64, centre_mean: f64, edge_mean: f64 }`; `luma_stats(frame: &Frame) -> LumaStats`; `EmitterEvidence { delta: f64, centre_weighted: bool, confident: bool }`; `verify_emitter(off: &[Frame], on: &[Frame], cfg: &EmitterVerifyCfg) -> EmitterEvidence`; `EmitterVerifyCfg { min_delta: f64, require_centre_weighted: bool }` with `Default`.

**Why this replaces asking a human:** `linux-enable-ir-emitter` asks the user whether a light came on, which is unreasonable for invisible 850 nm emission and drives people to point phone cameras at their laptops. Measuring the luminance delta closes the loop in software.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/emitter.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Automatic verification that an extension unit control drives the IR
//! emitter, by measuring the luminance change it causes.

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::recording::SessionLabel;
    use namtor_capture::replay::FileReplay;
    use namtor_capture::source::CaptureSource;
    use namtor_capture::synth::{synth_session, SynthGeometry, SynthOpts};

    /// Renders a synthetic session and splits it into off/on frame vectors.
    fn synth_pairs(geometry: SynthGeometry, pairs: u32) -> (Vec<Frame>, Vec<Frame>) {
        let dir = tempfile::tempdir().unwrap();
        synth_session(
            dir.path(),
            &SynthOpts {
                pairs,
                seed: 5,
                geometry,
                label: SessionLabel::Synthetic,
            },
        )
        .unwrap();

        let mut r = FileReplay::open(dir.path()).unwrap();
        r.start().unwrap();

        let (mut on, mut off) = (Vec::new(), Vec::new());
        while let Some(f) = r.next_frame().unwrap() {
            match f.emitter {
                namtor_capture::frame::EmitterState::On => on.push(f),
                _ => off.push(f),
            }
        }
        (off, on)
    }

    #[test]
    fn detects_a_real_luminance_increase() {
        let (off, on) = synth_pairs(SynthGeometry::Face, 3);
        let ev = verify_emitter(&off, &on, &EmitterVerifyCfg::default());
        assert!(ev.delta > 10.0, "expected a clear delta, got {}", ev.delta);
        assert!(ev.confident);
    }

    #[test]
    fn identical_frame_sets_produce_no_evidence() {
        let (off, _) = synth_pairs(SynthGeometry::Face, 3);
        let ev = verify_emitter(&off, &off, &EmitterVerifyCfg::default());
        assert!(ev.delta.abs() < 2.0);
        assert!(!ev.confident, "must not claim an emitter with no delta");
    }

    #[test]
    fn face_geometry_is_recognised_as_centre_weighted() {
        // A point emitter illuminates the centre more than the edges. A
        // control that raises the whole image uniformly is more likely a gain
        // or exposure control than an emitter.
        let (off, on) = synth_pairs(SynthGeometry::Face, 2);
        let ev = verify_emitter(&off, &on, &EmitterVerifyCfg::default());
        assert!(ev.centre_weighted);
    }

    #[test]
    fn uniform_brightening_is_not_centre_weighted() {
        let (off, on) = synth_pairs(SynthGeometry::Flat, 2);
        let ev = verify_emitter(&off, &on, &EmitterVerifyCfg::default());
        assert!(!ev.centre_weighted);
    }

    #[test]
    fn centre_weighting_can_be_required_or_relaxed() {
        // Some emitters are diffuse enough to brighten uniformly at close
        // range, so the requirement is configurable rather than absolute.
        let (off, on) = synth_pairs(SynthGeometry::Flat, 2);

        let strict = verify_emitter(
            &off, &on,
            &EmitterVerifyCfg { min_delta: 10.0, require_centre_weighted: true },
        );
        assert!(!strict.confident);

        let relaxed = verify_emitter(
            &off, &on,
            &EmitterVerifyCfg { min_delta: 10.0, require_centre_weighted: false },
        );
        assert!(relaxed.confident);
    }

    #[test]
    fn empty_frame_sets_are_not_confident() {
        let ev = verify_emitter(&[], &[], &EmitterVerifyCfg::default());
        assert!(!ev.confident);
    }
}
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod emitter;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc emitter::`
Expected: FAIL — `cannot find function verify_emitter in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/emitter.rs` above the `tests` module:

```rust
use namtor_capture::frame::{Frame, PixFmt};

/// Fraction of the image half-width treated as "centre" for the radial test.
const CENTRE_FRACTION: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LumaStats {
    pub mean: f64,
    pub centre_mean: f64,
    pub edge_mean: f64,
}

/// Mean luminance overall, in the centre region, and in the outer region.
///
/// Only meaningful for 8-bit greyscale; other formats yield zeroes.
pub fn luma_stats(frame: &Frame) -> LumaStats {
    if frame.format != PixFmt::Grey8 || frame.data.is_empty() {
        return LumaStats { mean: 0.0, centre_mean: 0.0, edge_mean: 0.0 };
    }

    let w = frame.width as usize;
    let h = frame.height as usize;
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let max_r = (cx * cx + cy * cy).sqrt();

    let (mut total, mut centre_sum, mut centre_n, mut edge_sum, mut edge_n) =
        (0.0f64, 0.0f64, 0usize, 0.0f64, 0usize);

    for y in 0..h {
        for x in 0..w {
            let Some(&v) = frame.data.get(y * w + x) else {
                continue;
            };
            let v = v as f64;
            total += v;

            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let r = (dx * dx + dy * dy).sqrt() / max_r;

            if r < CENTRE_FRACTION {
                centre_sum += v;
                centre_n += 1;
            } else if r > 1.0 - CENTRE_FRACTION {
                edge_sum += v;
                edge_n += 1;
            }
        }
    }

    let n = (w * h) as f64;
    LumaStats {
        mean: total / n,
        centre_mean: if centre_n > 0 { centre_sum / centre_n as f64 } else { 0.0 },
        edge_mean: if edge_n > 0 { edge_sum / edge_n as f64 } else { 0.0 },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmitterVerifyCfg {
    /// Minimum mean luminance increase to accept, in 8-bit levels.
    pub min_delta: f64,
    /// Require the increase to be stronger at the centre than at the edges.
    pub require_centre_weighted: bool,
}

impl Default for EmitterVerifyCfg {
    fn default() -> Self {
        Self {
            min_delta: 10.0,
            require_centre_weighted: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmitterEvidence {
    /// Mean luminance increase from off-set to on-set.
    pub delta: f64,
    /// Whether the increase is stronger at the centre than at the edges,
    /// as a point emitter's inverse-square falloff implies.
    pub centre_weighted: bool,
    /// Whether the evidence satisfies the configured thresholds.
    pub confident: bool,
}

fn mean_of(frames: &[Frame], pick: fn(&LumaStats) -> f64) -> f64 {
    if frames.is_empty() {
        return 0.0;
    }
    frames.iter().map(|f| pick(&luma_stats(f))).sum::<f64>() / frames.len() as f64
}

/// Decides whether a control drove the IR emitter, from frames captured
/// before and after it was set.
pub fn verify_emitter(
    off: &[Frame],
    on: &[Frame],
    cfg: &EmitterVerifyCfg,
) -> EmitterEvidence {
    if off.is_empty() || on.is_empty() {
        return EmitterEvidence {
            delta: 0.0,
            centre_weighted: false,
            confident: false,
        };
    }

    let delta = mean_of(on, |s| s.mean) - mean_of(off, |s| s.mean);

    let centre_delta = mean_of(on, |s| s.centre_mean) - mean_of(off, |s| s.centre_mean);
    let edge_delta = mean_of(on, |s| s.edge_mean) - mean_of(off, |s| s.edge_mean);
    // Require a clear margin so sensor noise cannot flip the verdict.
    let centre_weighted = centre_delta > edge_delta + 5.0;

    let confident =
        delta >= cfg.min_delta && (!cfg.require_centre_weighted || centre_weighted);

    EmitterEvidence {
        delta,
        centre_weighted,
        confident,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc emitter::`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/emitter.rs crates/namtor-uvc/src/lib.rs
git commit -m "feat(uvc): automatic emitter verification by luminance delta"
```

---

### Task 8: Live device transport

**Files:**
- Create: `crates/namtor-uvc/src/device.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: `XuTransport`, `Query`, `XuError` from Tasks 1–3
- Produces: `V4l2Device::open(path: &Path) -> Result<V4l2Device, XuError>` implementing `XuTransport` via `UVCIOC_CTRL_QUERY`; `V4l2Device::fd(&self) -> RawFd`.

**Hardware-gated:** the live test is `#[ignore]` and requires `NAMTOR_HW_TEST=1`.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/device.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Live V4L2 device transport for extension unit queries.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::SafeXu;
    use std::path::Path;

    fn hw_enabled() -> bool {
        std::env::var("NAMTOR_HW_TEST").is_ok()
    }

    #[test]
    fn opening_a_nonexistent_device_is_an_error() {
        assert!(V4l2Device::open(Path::new("/dev/video-does-not-exist")).is_err());
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn reference_camera_ir_node_opens() {
        if !hw_enabled() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let dev = V4l2Device::open(Path::new("/dev/video2"))
            .expect("IR node /dev/video2 must be openable; check `video` group membership");
        assert!(dev.fd() >= 0);
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn xu_query_returns_either_data_or_a_clean_error_never_a_hang() {
        // Answers a real unknown: whether unmapped XU access needs elevated
        // privilege on this kernel. EPERM here is a finding, not a failure.
        if !hw_enabled() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let dev = V4l2Device::open(Path::new("/dev/video2")).unwrap();
        let mut xu = SafeXu::new(dev);

        // Unit 4 is the MSXU on the IR VideoControl interface, and it
        // advertises selectors 6 and 9 (bmControls 20 01). These are the
        // primary emitter candidates on this device.
        for selector in [6u8, 9u8] {
            match xu.probe_control(4, selector) {
                Ok(p) => eprintln!(
                    "unit 4 selector {selector}: len={} info={:?} current={:02x?}",
                    p.len, p.info, p.current
                ),
                Err(e) => eprintln!("unit 4 selector {selector}: {e}"),
            }
        }
    }
}
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod device;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc device::`
Expected: FAIL — `cannot find struct V4l2Device in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/device.rs` above the `tests` module:

```rust
use crate::probe::{XuError, XuTransport};
use crate::xu::{uvc_ctrl_query, Query, UvcXuControlQuery};
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

/// A V4L2 device node, used only as an ioctl target for XU queries.
pub struct V4l2Device {
    file: File,
}

impl V4l2Device {
    pub fn open(path: &Path) -> Result<Self, XuError> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| XuError::Ioctl(format!("open {}: {e}", path.display())))?;
        Ok(Self { file })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl XuTransport for V4l2Device {
    fn query(
        &self,
        unit: u8,
        selector: u8,
        query: Query,
        buf: &mut [u8],
    ) -> Result<(), XuError> {
        let mut q = UvcXuControlQuery {
            unit,
            selector,
            query: query as u8,
            size: buf.len() as u16,
            data: buf.as_mut_ptr().cast(),
        };

        // SAFETY: `q.data` points at `buf`, which outlives this call, and
        // `q.size` is exactly `buf.len()`. The kernel reads or writes at most
        // `size` bytes there.
        let rc = unsafe { uvc_ctrl_query(self.fd(), &mut q) };

        rc.map(|_| ())
            .map_err(|e| XuError::Ioctl(format!("UVCIOC_CTRL_QUERY {query:?}: {e}")))
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc device::`
Expected: PASS — 1 test runs, 2 ignored.

- [ ] **Step 5: Run the hardware tests on the reference machine**

Run: `NAMTOR_HW_TEST=1 cargo test -p namtor-uvc device:: -- --ignored --nocapture --test-threads=1`
Expected: `/dev/video2` opens. The XU query prints either a control profile or a clean error. **Record the exact output — it is spike finding R3-a.**

If this reports `EPERM`, note it: unmapped XU access requires elevated privilege on this kernel, which changes the daemon's capability requirements in M3.

- [ ] **Step 6: Commit**

```bash
git add crates/namtor-uvc/src/device.rs crates/namtor-uvc/src/lib.rs
git commit -m "feat(uvc): live V4L2 device transport for XU queries"
```

---

### Task 9: Emitter discovery driver

**Files:**
- Create: `crates/namtor-uvc/src/discover.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`

**Interfaces:**
- Consumes: `SafeXu`, `verify_emitter`, `EmitterVerifyCfg`, `ExtensionUnit`
- Produces: `Candidate { unit: u8, selector: u8, payload_on: Vec<u8>, payload_off: Vec<u8>, evidence: EmitterEvidence }`; `trait FrameSampler { fn sample(&mut self, n: usize) -> Result<Vec<Frame>, XuError>; }`; `discover_emitter<T, S>(xu: &mut SafeXu<T>, sampler: &mut S, units: &[ExtensionUnit], cfg: &DiscoverCfg) -> Vec<Candidate>`; `DiscoverCfg { selectors: Vec<u8>, sample_frames: usize, verify: EmitterVerifyCfg }`.

**Search strategy:** for each unit and selector, probe the control. Skip if it has no SET support. Read the current value, then try the two most likely payloads — all-ones and all-zeroes with the low bit set — restoring after each. A candidate is recorded when the luminance evidence is confident.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/discover.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Automatic emitter discovery across extension unit controls.

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

    /// A fake camera in which exactly one control drives brightness.
    ///
    /// Shared through `Rc` so the transport and the frame sampler observe the
    /// *same* device state — otherwise the positive test cannot succeed and
    /// the negative test proves nothing.
    struct FakeCamera {
        controls: RefCell<HashMap<(u8, u8), (u16, u8, Vec<u8>)>>,
        magic: (u8, u8),
    }

    impl FakeCamera {
        fn new(magic: (u8, u8)) -> Self {
            let mut m = HashMap::new();
            // Two writable controls and one read-only control.
            m.insert((3, 0x01), (1u16, 0b11u8, vec![0x00]));
            m.insert((3, 0x02), (1u16, 0b11u8, vec![0x00]));
            m.insert((3, 0x03), (1u16, 0b01u8, vec![0x00]));
            Self { controls: RefCell::new(m), magic }
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
            &self, unit: u8, selector: u8, query: Query, buf: &mut [u8],
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
        let mut sampler = FakeSampler { cam: Rc::clone(&cam) };

        let found = discover_emitter(
            &mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default(),
        );

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
        let mut sampler = FakeSampler { cam: Rc::clone(&cam) };

        let found = discover_emitter(
            &mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default(),
        );

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
        let mut sampler = FakeSampler { cam: Rc::clone(&cam) };

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
        let mut sampler = FakeSampler { cam: Rc::clone(&cam) };

        let found = discover_emitter(
            &mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default(),
        );

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

        let found = discover_emitter(
            &mut xu, &mut sampler, &[unit(3)], &DiscoverCfg::default(),
        );
        assert!(found.is_empty(), "must not report an emitter that isn't there");
    }

    #[test]
    fn unknown_units_are_skipped_without_error() {
        let cam = Rc::new(FakeCamera::new((3, 0x02)));
        let mut xu = SafeXu::new(Rc::clone(&cam));
        let mut sampler = ConstSampler { level: 40 };

        let found = discover_emitter(
            &mut xu, &mut sampler, &[unit(99)], &DiscoverCfg::default(),
        );
        assert!(found.is_empty());
    }
}
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod discover;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc discover::`
Expected: FAIL — `cannot find function discover_emitter in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/discover.rs` above the `tests` module:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc discover::`
Expected: PASS — 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/discover.rs crates/namtor-uvc/src/lib.rs
git commit -m "feat(uvc): automatic emitter discovery driver"
```

---

### Task 10: Spike CLI — R3, emitter controllability

**Files:**
- Create: `crates/namtor-uvc/src/bin/namtor-spike.rs`
- Modify: `crates/namtor-uvc/Cargo.toml`

**Interfaces:**
- Consumes: everything from Tasks 1–9
- Produces: binary `namtor-spike emitter --device /dev/video2 --descriptors /sys/bus/usb/devices/3-8/descriptors [--yes]` printing discovered candidates, and `namtor-spike descriptors --path <file>` dumping parsed units and formats.

**This task answers R3** — the single greatest threat to the project. If no control drives the emitter, strobing is impossible and the liveness design must be revisited.

Frame sampling here uses the V4L2 streaming API. To keep the spike small, sampling reads from the device with `read()` where supported and falls back to a clear error otherwise; full mmap streaming lands in M2's capture layer.

- [ ] **Step 1: Add the binary declaration and dependency**

Add to `crates/namtor-uvc/Cargo.toml`:

```toml
[[bin]]
name = "namtor-spike"
path = "src/bin/namtor-spike.rs"
```

- [ ] **Step 2: Write the CLI**

Create `crates/namtor-uvc/src/bin/namtor-spike.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Hardware spike CLI. Answers the M1 open risks against a real camera.
//!
//! WARNING: `emitter` writes to vendor extension unit controls. Every write is
//! preceded by a read and followed by a restore, but a non-compliant device
//! can still misbehave. Requires explicit confirmation.

use namtor_uvc::descriptors::{format_guid, is_l8_ir_format, parse_descriptors};
use namtor_uvc::device::V4l2Device;
use namtor_uvc::discover::{discover_emitter, DiscoverCfg, FrameSampler};
use namtor_uvc::probe::{SafeXu, XuError};
use namtor_capture::frame::{EmitterState, Frame, PixFmt};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// Samples frames by shelling out to `v4l2-ctl --stream-mmap`.
///
/// Deliberately not the V4L2 `read()` API: most UVC devices support only
/// mmap streaming, and `read()` would fail on them — which would block the
/// single most important measurement in the project. Subprocess overhead of
/// a few hundred milliseconds per sample is irrelevant for a spike run a few
/// dozen times. M2's capture layer does mmap streaming in-process.
struct V4l2CtlSampler {
    device: PathBuf,
    width: u32,
    height: u32,
    started: Instant,
    scratch: PathBuf,
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

fn cmd_descriptors(path: PathBuf) -> ExitCode {
    let blob = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let set = parse_descriptors(&blob);

    println!("Extension units: {}", set.extension_units.len());
    for xu in &set.extension_units {
        println!(
            "  if{} unit {:>3}  {}  bmControls={:02x?}",
            xu.interface,
            xu.unit_id,
            format_guid(&xu.guid),
            xu.bm_controls
        );
    }

    println!("Formats: {}", set.formats.len());
    for f in &set.formats {
        println!(
            "  if{} index {}  {}{}",
            f.interface,
            f.format_index,
            format_guid(&f.guid),
            if is_l8_ir_format(&f.guid) { "   <- IR (L8)" } else { "" }
        );
    }
    ExitCode::SUCCESS
}

fn cmd_emitter(
    device: PathBuf,
    descriptors: PathBuf,
    ir_vc_interface: u8,
    assume_yes: bool,
) -> ExitCode {
    if !assume_yes {
        println!(
            "This probes vendor extension unit controls by writing to them.\n\
             Every write is read first and restored after, but a non-compliant\n\
             device may still hang and require a USB reset or reboot.\n\
             Continue? [y/N] "
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || !answer.trim().eq_ignore_ascii_case("y")
        {
            println!("aborted");
            return ExitCode::SUCCESS;
        }
    }

    let blob = match std::fs::read(&descriptors) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", descriptors.display());
            return ExitCode::FAILURE;
        }
    };
    let set = parse_descriptors(&blob);

    // CRITICAL: a multi-sensor camera declares one set of extension units per
    // VideoControl interface. Probing the colour sensor's units would poke the
    // wrong device. Keep only the IR sensor's VideoControl interface.
    const MSXU_PREFIX: &str = "0f3f95dc-2632-4c4e";
    let mut units: Vec<_> = set
        .extension_units
        .iter()
        .filter(|xu| xu.interface == ir_vc_interface)
        .cloned()
        .collect();

    // MSXU first: standardised semantics, so tier 1 probes it silently
    // (spec §4.10.1). Vendor units are the consented tier-2 fallback.
    units.sort_by_key(|xu| !format_guid(&xu.guid).starts_with(MSXU_PREFIX));

    println!(
        "probing {} extension unit(s) on VideoControl interface {}:",
        units.len(),
        ir_vc_interface
    );
    for xu in &units {
        println!(
            "  unit {:<3} {}  advertised selectors {:?}",
            xu.unit_id,
            format_guid(&xu.guid),
            xu.advertised_selectors()
        );
    }

    let transport = match V4l2Device::open(&device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut xu = SafeXu::new(transport);

    if Command::new("v4l2-ctl").arg("--version").output().is_err() {
        eprintln!("error: v4l2-ctl not found (pkg: v4l-utils); frame sampling needs it");
        return ExitCode::FAILURE;
    }

    let mut sampler = V4l2CtlSampler {
        device: device.clone(),
        width: 640,
        height: 360,
        started: Instant::now(),
        scratch: std::env::temp_dir().join("namtor-spike-frames.raw"),
    };

    let started = Instant::now();
    let found = discover_emitter(&mut xu, &mut sampler, &units, &DiscoverCfg::default());
    let elapsed = started.elapsed();

    println!("\n=== R3: emitter controllability ===");
    if found.is_empty() {
        println!("NO EMITTER CONTROL FOUND after {elapsed:?}");
        println!("This is spike finding R3-FAIL. Liveness design must be revisited.");
        return ExitCode::FAILURE;
    }
    for c in &found {
        println!(
            "unit {} selector {:#04x}  delta={:.1}  centre_weighted={}  \
             on={:02x?} off={:02x?}",
            c.unit,
            c.selector,
            c.evidence.delta,
            c.evidence.centre_weighted,
            c.payload_on,
            c.payload_off
        );
    }
    println!("R3-PASS in {elapsed:?}");
    ExitCode::SUCCESS
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         namtor-spike descriptors --path <file>\n  \
         namtor-spike emitter --device <node> --descriptors <file> \
         [--ir-vc-interface N] [--yes]\n\n\
         --ir-vc-interface defaults to 2 (the reference camera's IR\n\
         VideoControl interface). Find it with `namtor-spike descriptors`:\n\
         it is the interface whose extension units accompany the L8 format."
    );
    std::process::exit(2);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    let mut path: Option<PathBuf> = None;
    let mut device: Option<PathBuf> = None;
    let mut descriptors: Option<PathBuf> = None;
    let mut ir_vc_interface: u8 = 2;
    let mut assume_yes = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--path" => { i += 1; path = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage()))); }
            "--device" => { i += 1; device = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage()))); }
            "--descriptors" => { i += 1; descriptors = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage()))); }
            "--ir-vc-interface" => {
                i += 1;
                ir_vc_interface = args
                    .get(i)
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
            }
            "--yes" => assume_yes = true,
            _ => usage(),
        }
        i += 1;
    }

    match args[0].as_str() {
        "descriptors" => cmd_descriptors(path.unwrap_or_else(|| usage())),
        "emitter" => cmd_emitter(
            device.unwrap_or_else(|| usage()),
            descriptors.unwrap_or_else(|| usage()),
            ir_vc_interface,
            assume_yes,
        ),
        _ => usage(),
    }
}
```

- [ ] **Step 3: Verify the descriptors subcommand against the fixture**

Run:

```bash
cargo run -p namtor-uvc --bin namtor-spike -- \
    descriptors --path fixtures/descriptors/3277-0018.bin
```

Expected: **four** extension units — `if0 unit 3` (`bf`), `if0 unit 4` (`0000`), `if2 unit 3` (`00`), `if2 unit 4` (`2001`) — and two formats, with the L8 one on `if3` annotated `<- IR (L8)`.

The IR VideoControl interface is `if2`: its extension units accompany the L8 streaming interface (`if3`). Pass `--ir-vc-interface 2` to the `emitter` subcommand, which is also its default.

- [ ] **Step 4: Run the emitter discovery against the real camera**

Run:

```bash
cargo run -p namtor-uvc --bin namtor-spike -- \
    emitter --device /dev/video2 \
            --descriptors /sys/bus/usb/devices/3-8/descriptors
```

Expected: either a list of candidate controls with luminance deltas (**R3-PASS**), or `NO EMITTER CONTROL FOUND` (**R3-FAIL**).

**Record the full output verbatim.** This is the project's most consequential measurement.

Troubleshooting:

- **`v4l2-ctl failed: ... Device or resource busy`** — something else holds the camera. Close any application using it, and check with `fuser -v /dev/video2`.
- **`UVCIOC_CTRL_QUERY ...: EPERM`** — unmapped extension unit access needs elevated privilege on this kernel. Re-run under `sudo` to get the measurement, and **record it as a finding**: it changes the daemon's capability requirements in M3.
- **`captured N bytes, need at least ...`** — the negotiated format differs from 640×360 GREY. Check `v4l2-ctl -d /dev/video2 --list-formats-ext` and adjust the sampler's geometry.
- **Every candidate reports `delta` near zero** — the emitter may be strobing too briefly to register, or the control latches only while streaming. Note it; this is the case where §4.10.5's `firmware_strobed` or a stream-scoped control is in play.

- [ ] **Step 5: Commit**

```bash
git add crates/namtor-uvc/src/bin/namtor-spike.rs crates/namtor-uvc/Cargo.toml
git commit -m "feat(uvc): spike CLI for emitter discovery and descriptor dump"
```

---

### Task 11: Dual-stream bandwidth measurement — R1

**Files:**
- Create: `scripts/spike-dual-stream.sh`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces: a script that attempts simultaneous IR and RGB capture and reports whether UVC bandwidth negotiation permits it.

**This answers R1.** Arithmetic says two streams fit in USB 2.0's practical ceiling, but devices reserve conservatively and often reject a second concurrent alt-setting regardless of headroom.

- [ ] **Step 1: Write the script**

Create `scripts/spike-dual-stream.sh`:

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# M1 spike, risk R1: can the reference camera stream IR and RGB at once?
#
# Both sensors sit on one USB device sharing one USB 2.0 bus. If negotiation
# refuses the second stream, dual-camera fusion must fall back to alternating
# bursts (spec §4.8).
set -uo pipefail

IR_NODE="${IR_NODE:-/dev/video2}"
RGB_NODE="${RGB_NODE:-/dev/video0}"
FRAMES="${FRAMES:-60}"

command -v v4l2-ctl >/dev/null || { echo "v4l2-ctl not found (pkg: v4l-utils)"; exit 1; }

echo "=== R1: dual-stream bandwidth ==="
echo "IR:  $IR_NODE"
echo "RGB: $RGB_NODE"
echo

echo "--- baseline: IR alone ---"
v4l2-ctl -d "$IR_NODE" --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null 2>&1 | tail -3
ir_alone=$?
echo "IR alone exit: $ir_alone"
echo

echo "--- baseline: RGB alone ---"
v4l2-ctl -d "$RGB_NODE" --set-fmt-video=width=1280,height=720,pixelformat=MJPG \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null 2>&1 | tail -3
rgb_alone=$?
echo "RGB alone exit: $rgb_alone"
echo

echo "--- simultaneous ---"
v4l2-ctl -d "$IR_NODE" --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null >/tmp/namtor-ir.log 2>&1 &
ir_pid=$!

sleep 0.3

v4l2-ctl -d "$RGB_NODE" --set-fmt-video=width=1280,height=720,pixelformat=MJPG \
    --stream-mmap --stream-count="$FRAMES" --stream-to=/dev/null >/tmp/namtor-rgb.log 2>&1 &
rgb_pid=$!

wait "$ir_pid"; ir_both=$?
wait "$rgb_pid"; rgb_both=$?

echo "IR  concurrent exit: $ir_both"
sed 's/^/    ir:  /' /tmp/namtor-ir.log | tail -5
echo "RGB concurrent exit: $rgb_both"
sed 's/^/    rgb: /' /tmp/namtor-rgb.log | tail -5
echo

echo "=== VERDICT ==="
if [ "$ir_both" -eq 0 ] && [ "$rgb_both" -eq 0 ]; then
    echo "R1-PASS: simultaneous IR + RGB capture succeeded."
    echo "Dual-camera fusion (spec §6.5) can use concurrent streams."
else
    echo "R1-FAIL: concurrent capture refused."
    echo "Fusion must use alternating bursts (spec §4.8 fallback)."
    echo "Check logs above for ENOSPC / 'Cannot allocate memory' — the usual"
    echo "signature of UVC bandwidth negotiation refusing the second stream."
fi
```

Make it executable:

```bash
chmod +x scripts/spike-dual-stream.sh
```

- [ ] **Step 2: Run it on the reference machine**

Run: `./scripts/spike-dual-stream.sh`
Expected: a verdict line, `R1-PASS` or `R1-FAIL`.

**Record the full output.** If `R1-FAIL`, note the exact error — `ENOSPC` and `Cannot allocate memory` are the usual bandwidth-negotiation refusals, and distinguishing them from a permissions problem matters.

- [ ] **Step 3: Commit**

```bash
git add scripts/spike-dual-stream.sh
git commit -m "feat(spike): dual-stream bandwidth measurement for R1"
```

---

### Task 12: Auto-exposure behaviour measurement — R2

**Files:**
- Create: `scripts/spike-ae-hunting.sh`

**Interfaces:**
- Consumes: `namtor-spike` from Task 10
- Produces: a script recording IR frame luminance across a forced emitter toggle, so AE hunting is visible in the data.

**This answers R2.** The IR node exposes no exposure or gain controls, so firmware AE is in charge. If AE hunts against the strobe — bright frame triggers exposure reduction, next dark frame under-exposes, oscillation — the strobe differential degrades and the learned PAD model must carry more weight.

**Prerequisite:** R3 must have passed. Without emitter control there is nothing to toggle.

- [ ] **Step 1: Write the script**

Create `scripts/spike-ae-hunting.sh`:

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# M1 spike, risk R2: does firmware auto-exposure hunt against the strobe?
#
# Captures a run of IR frames while the emitter is held on, then held off,
# and reports per-frame mean luminance. A stable emitter-on plateau means AE
# is not fighting us. A decaying ramp after the emitter turns on is AE hunting.
#
# Requires R3-PASS: emitter unit/selector/payloads from `namtor-spike emitter`.
set -uo pipefail

IR_NODE="${IR_NODE:-/dev/video2}"
FRAMES="${FRAMES:-90}"
OUT="${OUT:-/tmp/namtor-ae}"

command -v v4l2-ctl >/dev/null || { echo "v4l2-ctl not found (pkg: v4l-utils)"; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found"; exit 1; }

mkdir -p "$OUT"

echo "=== R2: auto-exposure behaviour ==="
echo "Capturing $FRAMES IR frames to $OUT"
echo
echo "Hold the emitter ON using the unit/selector recorded by:"
echo "  namtor-spike emitter --device $IR_NODE --descriptors <sysfs descriptors>"
echo "then press Enter to capture."
read -r _

v4l2-ctl -d "$IR_NODE" \
    --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" \
    --stream-to="$OUT/on.raw"

echo "Now turn the emitter OFF and press Enter."
read -r _

v4l2-ctl -d "$IR_NODE" \
    --set-fmt-video=width=640,height=360,pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" \
    --stream-to="$OUT/off.raw"

python3 - "$OUT" "$FRAMES" <<'PY'
import sys, pathlib

out = pathlib.Path(sys.argv[1])
n = int(sys.argv[2])
size = 640 * 360

def means(path):
    data = path.read_bytes()
    frames = len(data) // size
    return [
        sum(data[i * size:(i + 1) * size]) / size
        for i in range(min(frames, n))
    ]

on = means(out / "on.raw")
off = means(out / "off.raw")

print(f"\nemitter ON  frames: {len(on)}")
print("  first 10:", " ".join(f"{m:.1f}" for m in on[:10]))
print("  last 10: ", " ".join(f"{m:.1f}" for m in on[-10:]))
print(f"emitter OFF frames: {len(off)}")
print("  first 10:", " ".join(f"{m:.1f}" for m in off[:10]))
print("  last 10: ", " ".join(f"{m:.1f}" for m in off[-10:]))

if not on or not off:
    print("\nNo frames captured; cannot judge R2.")
    sys.exit(1)

# AE hunting shows up as the emitter-on mean drifting back toward the
# emitter-off mean as the firmware compensates.
on_early = sum(on[:10]) / len(on[:10])
on_late = sum(on[-10:]) / len(on[-10:])
off_mean = sum(off) / len(off)

drift = on_early - on_late
separation = on_late - off_mean

print(f"\non_early={on_early:.1f} on_late={on_late:.1f} off_mean={off_mean:.1f}")
print(f"drift={drift:.1f}  separation_at_end={separation:.1f}")

print("\n=== VERDICT ===")
if drift > 15.0:
    print("R2-FAIL: emitter-on luminance decays; firmware AE is hunting.")
    print("Mitigations, in order (spec §4.5):")
    print("  1. find an exposure control via MSXU or vendor XU and pin it")
    print("  2. shorten strobe periods below AE's tracking rate")
    print("  3. normalise the differential in software, accepting noise")
elif separation < 10.0:
    print("R2-INCONCLUSIVE: emitter-on and emitter-off are barely separable.")
    print("Re-check that the emitter was actually held on.")
else:
    print("R2-PASS: emitter-on luminance is stable and well separated.")
    print("Strobe differential is viable as designed.")
PY
```

Make it executable:

```bash
chmod +x scripts/spike-ae-hunting.sh
```

- [ ] **Step 2: Run it on the reference machine**

Run: `./scripts/spike-ae-hunting.sh`
Expected: a verdict of `R2-PASS`, `R2-FAIL`, or `R2-INCONCLUSIVE`, with the luminance series printed.

**Record the full output including the numeric series** — the shape of the decay, not just the verdict, determines which mitigation applies.

- [ ] **Step 3: Commit**

```bash
git add scripts/spike-ae-hunting.sh
git commit -m "feat(spike): auto-exposure hunting measurement for R2"
```

---

### Task 13: Emitter-idle baseline — R9 and always-on detection

**Files:**
- Create: `scripts/spike-emitter-idle.sh`

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces: a script capturing IR frames with no emitter control applied, reporting mean luminance per frame, whether frames alternate (firmware strobing), and a verdict of `always_on`, `idle_dark`, or `firmware_strobed`.

**Why this exists:** spec §4.10.5. Some cameras energise the emitter whenever the IR stream opens, with no off control — on those, strobe differential is impossible. Others strobe in firmware, which would be free hardware-synchronised pairing. Both must be detected before any emitter probing, because both change what probing means.

**Reference result, already measured 2026-08-04.** The script must reproduce this on the reference machine:

```
frames=60
means: 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.4 0.4 ... 0.5 0.5
even-idx mean=0.38  odd-idx mean=0.40  diff=0.01
brightest frame: centre-strip=0.6 edge-strip=0.6
```

Verdict `idle_dark`: no firmware strobing, emitter not energised by streaming, and the sensor produces **no usable image at all** without the emitter. The slow 0.0 → 0.5 ramp is auto-exposure searching for light over ~2.4 s — orders of magnitude slower than a 33 ms strobe period, which is favourable preliminary evidence for R2.

- [ ] **Step 1: Write the script**

Create `scripts/spike-emitter-idle.sh`:

```bash
#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# M1 spike, risk R9: is the emitter energised by streaming alone, and does the
# firmware strobe it? Must run BEFORE any emitter probing, because both cases
# change what probing means (spec §4.10.5).
set -uo pipefail

IR_NODE="${IR_NODE:-/dev/video2}"
FRAMES="${FRAMES:-60}"
WIDTH="${WIDTH:-640}"
HEIGHT="${HEIGHT:-360}"
RAW="${RAW:-/tmp/namtor-idle.raw}"

command -v v4l2-ctl >/dev/null || { echo "v4l2-ctl not found (pkg: v4l-utils)"; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found"; exit 1; }

echo "=== R9: emitter idle baseline ==="
echo "Capturing $FRAMES frames from $IR_NODE with no emitter control applied."
echo "Leave the scene normally lit; do not cover the camera."
echo

v4l2-ctl -d "$IR_NODE" \
    --set-fmt-video=width="$WIDTH",height="$HEIGHT",pixelformat=GREY \
    --stream-mmap --stream-count="$FRAMES" --stream-to="$RAW" 2>&1 | tail -2

python3 - "$RAW" "$WIDTH" "$HEIGHT" <<'PY'
import sys, pathlib

raw = pathlib.Path(sys.argv[1])
w, h = int(sys.argv[2]), int(sys.argv[3])
size = w * h

data = raw.read_bytes()
n = len(data) // size
if n == 0:
    print("no frames captured")
    sys.exit(1)

means = [sum(data[i * size:(i + 1) * size]) / size for i in range(n)]
even = [m for i, m in enumerate(means) if i % 2 == 0]
odd = [m for i, m in enumerate(means) if i % 2 == 1]

even_m = sum(even) / len(even)
odd_m = sum(odd) / len(odd)
alt = abs(even_m - odd_m)
overall = sum(means) / len(means)
ramp = means[-1] - means[0]

print(f"\nframes={n}")
print("means:", " ".join(f"{m:.1f}" for m in means))
print(f"\neven-idx mean={even_m:.2f}  odd-idx mean={odd_m:.2f}  alternation={alt:.2f}")
print(f"overall mean={overall:.2f}  min={min(means):.1f}  max={max(means):.1f}")
print(f"first-to-last ramp={ramp:+.2f}")

print("\n=== VERDICT ===")
if alt > 15.0:
    print("firmware_strobed: frames alternate in brightness.")
    print("The device strobes the emitter itself. Pair frames from metadata;")
    print("no emitter control may be needed at all (spec §4.10.5).")
elif overall > 30.0:
    print("always_on (PROVISIONAL): the image is bright with no control applied.")
    print("Strobe differential would be impossible unless an off control exists.")
    print("")
    print("CONFIRM BEFORE RECORDING THIS. Strong ambient infrared -- direct")
    print("sunlight, halogen or incandescent lamps, a space heater -- produces")
    print("the same signature. Re-run in a darkened room with no window and no")
    print("incandescent source. If the image goes dark, the verdict is")
    print("idle_dark and the brightness was ambient, not the emitter.")
else:
    print("idle_dark: the emitter is not energised by streaming.")
    print("Emitter control is REQUIRED; the sensor is unusable without it.")
    print("Emitter control is mandatory. On the reference camera this is\n"
          "supplied by MSXU mode 2 (Tier 0) -- see quirks/devices/3277-0018.toml.")

if abs(ramp) > 0.2 and overall < 5.0:
    print(f"\nNote: a slow monotonic ramp ({ramp:+.2f} over {n} frames) is")
    print("auto-exposure searching for light. Slow AE is favourable for R2:")
    print("it cannot track a strobe toggling in tens of milliseconds.")
PY
```

Make it executable:

```bash
chmod +x scripts/spike-emitter-idle.sh
```

- [ ] **Step 2: Run it and confirm it reproduces the reference result**

Run: `./scripts/spike-emitter-idle.sh`
Expected on the reference machine: verdict `idle_dark`, overall mean below 1.0, alternation below 1.0, and the auto-exposure note.

If the verdict differs from `idle_dark`, the reference measurement above was environment-dependent — record the new result and note the conditions.

- [ ] **Step 3: Commit**

```bash
git add scripts/spike-emitter-idle.sh
git commit -m "feat(spike): emitter idle baseline and always-on detection for R9"
```

---

### Task 14: Tier 0 — device-driven illumination (the Windows Hello path)

**Files:**
- Create: `crates/namtor-uvc/src/mode.rs`
- Modify: `crates/namtor-uvc/src/lib.rs`
- Modify: `crates/namtor-uvc/src/bin/namtor-spike.rs`

**Interfaces:**
- Consumes: `SafeXu`, `FrameSampler`, `ExtensionUnit`, `luma_stats`
- Produces: `ModeControl { unit, selector, len, current: Vec<u8>, max: Vec<u8>, default: Vec<u8> }`; `find_mode_controls(xu, units) -> Vec<ModeControl>`; `AlternationEvidence { alternation: f64, on_mean: f64, off_mean: f64, alternates: bool }`; `measure_alternation(frames: &[Frame]) -> AlternationEvidence`; `try_modes(xu, sampler, ctrl, cfg) -> Vec<(Vec<u8>, AlternationEvidence)>`.

**Why this task is the most valuable one in M1.** Windows Hello does not drive the emitter from userspace — it sets an MSXU mode control and the *firmware* alternates illuminated and clean IR frames, tagging them. That is why Hello has no torn frames, no auto-exposure fight, and no per-device strobe timing. If this camera can do the same, §4.4's torn-frame machinery is unnecessary and risk R2 disappears entirely.

**Read-only measurements already taken on the reference camera (2026-08-04):**

```
MSXU unit 4, IR VideoControl interface, /dev/video2
  selector 6  len=9  info=0x03 [GET,SET]
      CUR 01 03 01 00 00 00 00 00 00
      MAX 01 03 03 00 00 00 00 00 00
      DEF 01 03 01 00 00 00 00 00 00
      MIN 00 00 00 00 00 00 00 00 00
  selector 9  len=4  info=0x03 [GET,SET]
      CUR 01 00 00 00   MAX 01 00 00 00   DEF 00 00 00 00   RES 01 00 00 00

Sonix unit 3: no control responds to GET_LEN.
UVCIOC_CTRL_QUERY succeeded as an ordinary desktop user — no root, no CAP_SYS_ADMIN.
```

Selector 6 byte 1 is fixed at `0x03` across CUR, MAX and DEF — a capability bitmask advertising two modes. Byte 2 is the live mode field, currently `1`, maximum `3`. **Mode 2 has never been selected.**

**Do not assume MSXU control semantics.** We cannot verify the published specification from here, and vendors deviate. Determine behaviour empirically: set each accepted mode, capture a burst, measure whether frames alternate. A mode producing clean alternation *is* device-driven illumination, whatever it is nominally called.

- [ ] **Step 1: Write the failing test**

Create `crates/namtor-uvc/src/mode.rs`:

```rust
// SPDX-License-Identifier: Apache-2.0
//! Device-driven illumination: asking the camera to alternate the emitter
//! itself, which is what Windows Hello does.

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_capture::frame::{EmitterState, Frame, PixFmt};
    use std::time::Duration;

    fn frames(levels: &[u8]) -> Vec<Frame> {
        levels
            .iter()
            .enumerate()
            .map(|(i, &l)| Frame {
                data: vec![l; 64 * 64],
                format: PixFmt::Grey8,
                width: 64,
                height: 64,
                ts_mono: Duration::from_millis(33 * i as u64),
                sof: Some(i as u16),
                emitter: EmitterState::Unknown,
                quality: None,
            })
            .collect()
    }

    #[test]
    fn alternating_frames_are_detected() {
        let ev = measure_alternation(&frames(&[200, 40, 200, 40, 200, 40]));
        assert!(ev.alternates, "clear alternation must be detected");
        assert!(ev.alternation > 100.0);
        assert!(ev.on_mean > ev.off_mean);
    }

    #[test]
    fn steady_frames_do_not_alternate() {
        let ev = measure_alternation(&frames(&[120, 120, 120, 120, 120, 120]));
        assert!(!ev.alternates);
        assert!(ev.alternation < 5.0);
    }

    #[test]
    fn a_slow_ramp_is_not_alternation() {
        // Auto-exposure hunting produces a monotonic drift, not alternation.
        // Misreading it as alternation would report Tier 0 support falsely.
        let ev = measure_alternation(&frames(&[40, 60, 80, 100, 120, 140]));
        assert!(!ev.alternates, "a monotonic ramp is drift, not strobing");
    }

    #[test]
    fn either_phase_may_come_first() {
        // The burst may start on either phase; both must classify correctly.
        for seq in [[40u8, 200, 40, 200], [200, 40, 200, 40]] {
            let ev = measure_alternation(&frames(&seq));
            assert!(ev.alternates, "sequence {seq:?} should alternate");
            assert!(ev.on_mean > ev.off_mean);
        }
    }

    #[test]
    fn decaying_illuminated_frames_still_classify_correctly() {
        // Real reference-camera data: auto-exposure decays the illuminated
        // phase 217 -> 45 while clean frames hold at 0. A global brightness
        // threshold derived from the burst mean nearly misclassifies the
        // settled tail and would fail outright if AE settled lower.
        let seq: Vec<u8> = vec![
            0, 217, 0, 216, 0, 115, 0, 115, 0, 69,
            0, 69, 0, 54, 0, 53, 0, 50, 0, 49,
            0, 46, 0, 46, 0, 46, 0, 45, 0, 45,
        ];
        let ev = measure_alternation(&frames(&seq));

        assert!(ev.alternates, "strict alternation must survive AE decay");
        assert!(ev.off_mean < 1.0, "clean phase should stay near zero");
        assert!(ev.on_mean > 40.0, "illuminated phase should stay well above it");
    }

    #[test]
    fn too_few_frames_is_inconclusive() {
        assert!(!measure_alternation(&frames(&[200])).alternates);
        assert!(!measure_alternation(&[]).alternates);
    }

    #[test]
    fn mode_candidates_walk_the_field_up_to_its_maximum() {
        // Selector 6 on the reference camera: byte 2 is the mode field,
        // current 1, max 3. Candidates must cover 2 and 3, and must preserve
        // every other byte of the payload.
        let ctrl = ModeControl {
            unit: 4,
            selector: 6,
            len: 9,
            current: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            max: vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0],
            default: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
        };

        let cands = ctrl.mode_candidates();

        assert!(cands.contains(&vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]));
        assert!(cands.contains(&vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0]));
        assert!(
            !cands.contains(&ctrl.current),
            "the current value produces no state change to observe"
        );
        for c in &cands {
            assert_eq!(c.len(), 9);
            assert_eq!(c[0], 0x01, "fixed bytes must be preserved");
            assert_eq!(c[1], 0x03, "capability mask must be preserved");
        }
    }

    #[test]
    fn the_varying_byte_is_located_by_comparing_current_with_max() {
        // Bytes equal in CUR and MAX are fixed; the differing byte is the
        // field with a range. Hardcoding an offset would break on any other
        // vendor's payload layout.
        let ctrl = ModeControl {
            unit: 4,
            selector: 6,
            len: 9,
            current: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
            max: vec![0x01, 0x03, 0x03, 0, 0, 0, 0, 0, 0],
            default: vec![0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0],
        };
        assert_eq!(ctrl.mode_byte(), Some(2));
    }

    #[test]
    fn a_boolean_control_yields_its_opposite_value() {
        // Selector 9 on the reference camera: CUR 1, MAX 1, DEF 0.
        let ctrl = ModeControl {
            unit: 4,
            selector: 9,
            len: 4,
            current: vec![0x01, 0, 0, 0],
            max: vec![0x01, 0, 0, 0],
            default: vec![0x00, 0, 0, 0],
        };
        assert!(ctrl.mode_candidates().contains(&vec![0x00, 0, 0, 0]));
    }
}
```

Add to `crates/namtor-uvc/src/lib.rs`:

```rust
pub mod mode;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p namtor-uvc mode::`
Expected: FAIL — `cannot find struct ModeControl in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/namtor-uvc/src/mode.rs` above the `tests` module:

```rust
use crate::descriptors::ExtensionUnit;
use crate::emitter::luma_stats;
use crate::probe::{SafeXu, XuError, XuTransport};
use crate::xu::Query;
use namtor_capture::frame::Frame;

/// A control whose payload contains a bounded field — a mode selector.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeControl {
    pub unit: u8,
    pub selector: u8,
    pub len: usize,
    pub current: Vec<u8>,
    pub max: Vec<u8>,
    pub default: Vec<u8>,
}

impl ModeControl {
    /// Index of the byte that varies between CUR and MAX.
    ///
    /// Bytes equal in both are fixed — version numbers, capability masks —
    /// and must be preserved. Hardcoding an offset would break on any other
    /// vendor's payload layout.
    pub fn mode_byte(&self) -> Option<usize> {
        self.current
            .iter()
            .zip(self.max.iter())
            .position(|(c, m)| c != m)
    }

    /// Payloads to try: every value of the mode field from its minimum
    /// through its maximum, excluding the current value, with all other
    /// bytes preserved exactly.
    pub fn mode_candidates(&self) -> Vec<Vec<u8>> {
        let Some(idx) = self.mode_byte() else {
            // No varying byte. A boolean whose CUR equals MAX still has a
            // distinct DEF worth trying.
            return if self.default != self.current {
                vec![self.default.clone()]
            } else {
                Vec::new()
            };
        };

        let hi = self.max[idx];
        let mut out = Vec::new();
        for v in 0..=hi {
            if v == self.current[idx] {
                continue;
            }
            let mut p = self.current.clone();
            p[idx] = v;
            out.push(p);
        }
        out
    }
}

/// Reads CUR, MAX and DEF for the selectors a unit advertises, keeping those
/// that look like mode controls.
pub fn find_mode_controls<T: XuTransport>(
    xu: &mut SafeXu<T>,
    units: &[ExtensionUnit],
) -> Vec<ModeControl> {
    let mut out = Vec::new();

    for unit in units {
        for selector in unit.advertised_selectors() {
            let Ok(profile) = xu.probe_control(unit.unit_id, selector) else {
                continue;
            };
            if !profile.info.supports_set() || profile.len == 0 {
                continue;
            }

            let mut max = vec![0u8; profile.len];
            let mut default = vec![0u8; profile.len];
            if xu.transport_query(unit.unit_id, selector, Query::GetMax, &mut max).is_err() {
                continue;
            }
            let _ = xu.transport_query(unit.unit_id, selector, Query::GetDef, &mut default);

            out.push(ModeControl {
                unit: unit.unit_id,
                selector,
                len: profile.len,
                current: profile.current,
                max,
                default,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlternationEvidence {
    /// Mean luminance difference between the bright and dark phases.
    pub alternation: f64,
    pub on_mean: f64,
    pub off_mean: f64,
    /// Whether the burst shows genuine frame-to-frame alternation.
    pub alternates: bool,
}

/// Minimum luminance separation between phases to call it alternation.
const ALTERNATION_THRESHOLD: f64 = 20.0;

/// Detects firmware-driven strobing in a captured burst.
///
/// **Classification is by frame parity, not by a global brightness
/// threshold.** Measured reference data shows illuminated frames decaying
/// 217.6 → 45.3 under auto-exposure while clean frames hold at 0.0. A global
/// threshold derived from the burst mean nearly misclassifies the settled
/// illuminated frames, and would fail outright if AE settled lower.
///
/// The shape check comes first: strict 1:1 alternation means consecutive
/// deltas flip sign at nearly every step. A monotonic ramp — AE hunting with
/// no strobe — never does, and must not be reported as alternation.
pub fn measure_alternation(frames: &[Frame]) -> AlternationEvidence {
    let none = AlternationEvidence {
        alternation: 0.0,
        on_mean: 0.0,
        off_mean: 0.0,
        alternates: false,
    };
    if frames.len() < 4 {
        return none;
    }

    let means: Vec<f64> = frames.iter().map(|f| luma_stats(f).mean).collect();

    // Strict alternation flips the sign of the delta at (almost) every step.
    let signs: Vec<i8> = means
        .windows(2)
        .map(|w| (w[1] - w[0]).signum() as i8)
        .collect();
    let flips = signs.windows(2).filter(|s| s[0] != s[1] && s[0] != 0).count();
    // Allow two non-flips for noise and for a settled tail.
    let alternating_shape = flips + 2 >= signs.len().saturating_sub(1);

    // Split by parity, then decide which parity is the illuminated one.
    // Robust to the illuminated phase decaying under auto-exposure.
    let (even, odd): (Vec<f64>, Vec<f64>) = means
        .iter()
        .enumerate()
        .partition_map_parity();

    if even.is_empty() || odd.is_empty() {
        return none;
    }
    let even_mean = even.iter().sum::<f64>() / even.len() as f64;
    let odd_mean = odd.iter().sum::<f64>() / odd.len() as f64;

    let (on_mean, off_mean) = if odd_mean >= even_mean {
        (odd_mean, even_mean)
    } else {
        (even_mean, odd_mean)
    };
    let alternation = on_mean - off_mean;

    AlternationEvidence {
        alternation,
        on_mean,
        off_mean,
        alternates: alternating_shape && alternation >= ALTERNATION_THRESHOLD,
    }
}

/// Splits an indexed iterator of means into (even-index, odd-index) vectors.
trait ParityPartition {
    fn partition_map_parity(self) -> (Vec<f64>, Vec<f64>);
}

impl<'a, I: Iterator<Item = (usize, &'a f64)>> ParityPartition for I {
    fn partition_map_parity(self) -> (Vec<f64>, Vec<f64>) {
        let mut even = Vec::new();
        let mut odd = Vec::new();
        for (i, v) in self {
            if i % 2 == 0 {
                even.push(*v);
            } else {
                odd.push(*v);
            }
        }
        (even, odd)
    }
}
```

Add this accessor to `SafeXu` in `crates/namtor-uvc/src/probe.rs`, so mode discovery can issue read queries without duplicating the transport:

```rust
impl<T: XuTransport> SafeXu<T> {
    /// Issues a **read-only** query directly against the transport.
    ///
    /// Panics in debug builds if given a write query: writes must go through
    /// [`SafeXu::try_write`], which enforces the probe safety rules.
    pub fn transport_query(
        &self,
        unit: u8,
        selector: u8,
        query: Query,
        buf: &mut [u8],
    ) -> Result<(), XuError> {
        debug_assert!(!query.is_write(), "writes must use try_write");
        if query.is_write() {
            return Err(XuError::NoSetSupport { unit, selector });
        }
        self.transport.query(unit, selector, query, buf)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p namtor-uvc mode::`
Expected: PASS — 9 tests.

- [ ] **Step 5: Add the `modes` subcommand to the spike CLI**

Add to `crates/namtor-uvc/src/bin/namtor-spike.rs`:

```rust
fn cmd_modes(
    device: PathBuf,
    descriptors: PathBuf,
    ir_vc_interface: u8,
    assume_yes: bool,
) -> ExitCode {
    if !assume_yes {
        println!(
            "This sets the camera's MSXU mode control to each value it accepts,\n\
             capturing a burst after each to detect firmware-driven strobing.\n\
             Every value is restored after testing. A non-compliant device may\n\
             still need a USB replug.\n\
             Continue? [y/N] "
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || !answer.trim().eq_ignore_ascii_case("y")
        {
            println!("aborted");
            return ExitCode::SUCCESS;
        }
    }

    let blob = match std::fs::read(&descriptors) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", descriptors.display());
            return ExitCode::FAILURE;
        }
    };

    let units: Vec<_> = parse_descriptors(&blob)
        .extension_units
        .into_iter()
        .filter(|xu| xu.interface == ir_vc_interface)
        .collect();

    let transport = match V4l2Device::open(&device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut xu = SafeXu::new(transport);
    let mut sampler = V4l2CtlSampler {
        device: device.clone(),
        width: 640,
        height: 360,
        started: Instant::now(),
        scratch: std::env::temp_dir().join("namtor-spike-modes.raw"),
    };

    let controls = find_mode_controls(&mut xu, &units);
    println!("mode-like controls found: {}", controls.len());

    let mut winners = Vec::new();

    for ctrl in &controls {
        println!(
            "\nunit {} selector {} len {}  CUR {:02x?}  MAX {:02x?}",
            ctrl.unit, ctrl.selector, ctrl.len, ctrl.current, ctrl.max
        );

        // Baseline at the current value, so a device that already alternates
        // is not credited to a mode change.
        match sampler.sample(16) {
            Ok(f) => {
                let ev = measure_alternation(&f);
                println!(
                    "  baseline           alternation={:.1} alternates={}",
                    ev.alternation, ev.alternates
                );
            }
            Err(e) => println!("  baseline capture failed: {e}"),
        }

        for payload in ctrl.mode_candidates() {
            let Ok(restorer) = xu.try_write(ctrl.unit, ctrl.selector, &payload) else {
                println!("  {payload:02x?}  rejected by device");
                continue;
            };

            let frames = sampler.sample(16).unwrap_or_default();
            drop(restorer); // restores the previous value

            if frames.is_empty() {
                println!("  {payload:02x?}  no frames captured");
                continue;
            }

            let ev = measure_alternation(&frames);
            let luma = frames
                .iter()
                .map(|f| namtor_uvc::emitter::luma_stats(f).mean)
                .sum::<f64>()
                / frames.len() as f64;

            println!(
                "  {payload:02x?}  mean={luma:.1} alternation={:.1} on={:.1} off={:.1} \
                 alternates={}",
                ev.alternation, ev.on_mean, ev.off_mean, ev.alternates
            );

            if ev.alternates {
                winners.push((ctrl.clone(), payload.clone(), ev));
            }
        }
    }

    println!("\n=== TIER 0: device-driven illumination ===");
    if winners.is_empty() {
        println!("No mode produced firmware alternation.");
        println!("Fall back to Tier 1 (`namtor-spike emitter`): userspace strobe.");
        return ExitCode::SUCCESS;
    }
    for (ctrl, payload, ev) in &winners {
        println!(
            "TIER-0-PASS  unit {} selector {} payload {:02x?}  \
             alternation={:.1} (on {:.1} / off {:.1})",
            ctrl.unit, ctrl.selector, payload, ev.alternation, ev.on_mean, ev.off_mean
        );
    }
    println!("\nThe camera strobes the emitter itself, as it does for Windows Hello.");
    println!("Torn-frame handling (spec §4.4) is unnecessary and risk R2 is void.");
    ExitCode::SUCCESS
}
```

Wire it into `main`, alongside `descriptors` and `emitter`:

```rust
        "modes" => cmd_modes(
            device.unwrap_or_else(|| usage()),
            descriptors.unwrap_or_else(|| usage()),
            ir_vc_interface,
            assume_yes,
        ),
```

and extend `usage()`:

```rust
         namtor-spike modes --device <node> --descriptors <file> \
         [--ir-vc-interface N] [--yes]\n
```

- [ ] **Step 6: Run it against the real camera**

**Run this before `namtor-spike emitter`.** If Tier 0 works, Tier 1 is unnecessary.

```bash
cargo run -p namtor-uvc --bin namtor-spike -- \
    modes --device /dev/video2 \
          --descriptors /sys/bus/usb/devices/3-8/descriptors
```

Expected on the reference camera: selector 6 offers mode-field values 0, 2 and 3 (current is 1), and selector 9 offers `00 00 00 00`. Watch for a payload whose burst reports `alternates=true`.

**Record the output verbatim.** A `TIER-0-PASS` line is the best possible outcome in M1: it means the camera does what it does for Hello, and several sections of the spec become unnecessary rather than merely satisfied.

If every mode reports `alternates=false` but some raise mean luminance substantially, that mode is a **plain emitter enable** — Tier 1. Feed it to `namtor-spike emitter` as a known-good starting point.

- [ ] **Step 7: Commit**

```bash
git add crates/namtor-uvc/src/mode.rs crates/namtor-uvc/src/lib.rs \
        crates/namtor-uvc/src/probe.rs crates/namtor-uvc/src/bin/namtor-spike.rs
git commit -m "feat(uvc): tier-0 device-driven illumination discovery"
```

---

### Task 15: Spike report and quirks entry #1

**Files:**
- Create: `docs/spikes/2026-08-04-m1-hardware-findings.md`
- Create: `quirks/README.md`
- Create: `quirks/devices/3277-0018.toml`

**Interfaces:**
- Consumes: recorded output from Tasks 8, 10, 11, 12
- Produces: the spike report resolving R1/R2/R3, and the first quirks database entry.

- [ ] **Step 1: Write the spike report**

Create `docs/spikes/2026-08-04-m1-hardware-findings.md`, filling every `<paste>` with the verbatim output recorded earlier. **Do not summarise the tool output — paste it.** A future contributor with different hardware needs the raw shape of these results.

```markdown
# M1 Hardware Spike — Findings

Date: <date run>
Machine: ASUS ROG Flow X16 GV601VI, Arch Linux, kernel <uname -r>
Camera: Sonix 3277:0018, USB 3-8

## Tier 0 — device-driven illumination

Command:
```
cargo run -p namtor-uvc --bin namtor-spike -- \
    modes --device /dev/video2 --descriptors /sys/bus/usb/devices/3-8/descriptors
```

Output:
```
<paste verbatim>
```

**Verdict:** TIER-0-PASS / TIER-0-FAIL

**Already answered on the reference camera (2026-08-04): TIER-0-PASS.**
`sel 6` mode 2 (`01 03 02 00 00 00 00 00 00`) produces strict 1:1 alternation,
illuminated 81.9 vs clean 0.13. Modes 0, 1 and 3 are inert; selector 9 is
unrelated to the emitter. Re-run to reproduce, and confirm restoration.

**If PASS:** the camera strobes the emitter itself, as it does for Windows
Hello. Torn-frame handling (spec §4.4) becomes unnecessary, risk R2 is void, and
per-device strobe timing disappears. Record the winning unit, selector, and
payload in the quirks entry and skip the Tier 1 emitter search entirely.

**If FAIL:** proceed to Tier 1 below. If a mode raised mean luminance without
alternating, it is a plain emitter enable — feed it to `namtor-spike emitter` as
a known-good starting point rather than letting the search rediscover it.

Also record: does the IR metadata node offer `V4L2_META_FMT_UVC_MSXU_1_5` in
addition to `UVCH`? Check with `v4l2-ctl -d /dev/video3 --list-formats-meta`.
Spec §4.11.1 — MSXU 1.5 metadata may carry per-frame illumination tagging, and
its absence is a candidate upstream `uvcvideo` patch.

## R9 — Emitter idle baseline

Command: `./scripts/spike-emitter-idle.sh`

Output:
```
<paste verbatim>
```

**Verdict:** idle_dark / always_on / firmware_strobed

**Consequences:** `always_on` makes strobe differential impossible on this
device — record `method = "always_on"`, evaluate gate G4 separately, and
surface the reduced protection in the UI (spec §4.10.5). `firmware_strobed`
is the best case: pair frames from metadata and skip emitter control entirely.

## R3 — Emitter controllability

Command:
```
cargo run -p namtor-uvc --bin namtor-spike -- \
    emitter --device /dev/video2 --descriptors /sys/bus/usb/devices/3-8/descriptors
```

Output:
```
<paste verbatim>
```

**Verdict:** R3-PASS / R3-FAIL

**Consequences if FAIL:** strobing is impossible; spec §6.4 liveness reduces to
what a static IR image supports, which is approximately Howdy's position. M6
must be redesigned before it is planned.

## R1 — Dual-stream bandwidth

Command: `./scripts/spike-dual-stream.sh`

Output:
```
<paste verbatim>
```

**Verdict:** R1-PASS / R1-FAIL

**Consequences if FAIL:** dual-camera fusion (spec §6.5) uses alternating
bursts rather than concurrent streams, adding roughly 100 ms to fused
authentication. Fusion is not lost.

## R2 — Auto-exposure hunting

Command: `./scripts/spike-ae-hunting.sh`

Output:
```
<paste verbatim>
```

**Verdict:** R2-PASS / R2-FAIL / R2-INCONCLUSIVE

**Consequences if FAIL:** apply the spec §4.5 mitigations in order. Strobe
differential weakens and the learned PAD model (spec §10.3) carries more of
gate G4.

## Incidental findings

- XU access privilege: `<EPERM or success — from Task 8 Step 5>`
- V4L2 `read()` support on the IR node: `<supported / requires mmap>`
- Anything else observed that contradicts the spec.

## Design changes required

<One line per spec section needing revision, or "none".>
```

- [ ] **Step 2: Verify the quirks database README**

`quirks/README.md` **already exists** — it was written during project scaffolding and documents the schema, the `emitter.method` values, and the interface-numbers-not-device-nodes rule. Read it before writing the entry in Step 3, then confirm the entry you produce matches the documented schema.

No changes needed unless your device exercises a case the README does not describe, in which case extend the README too.

- [ ] **Step 3: Verify and extend quirks entry #1**

`quirks/devices/3277-0018.toml` **already exists** with measured Tier 0 values from 2026-08-04. Verify your run reproduces them, then fill the fields still marked `untested` — notably `[bandwidth].concurrent_ir_rgb` from Task 11. Do not overwrite measured values with guesses; if your measurement differs, record both and note the conditions.

The committed entry, for reference:

```toml
# SPDX-License-Identifier: Apache-2.0
# Nam-tor quirks entry #1 — the reference development device.

[device]
vid_pid      = "3277:0018"
name         = "Sonix USB2.0 FHD UVC WebCam"
laptop       = "ASUS ROG Flow X16 GV601VI"
uvc_version  = "1.00"

[nodes]
# Interface numbers from the USB descriptors, not V4L2 node numbers, which
# are allocation-order dependent and change between boots.
ir_interface   = 3
rgb_interface  = 1
ir_format_guid = "00000032-0002-0010-8000-00aa00389b71"  # MFVideoFormat_L8
metadata_node  = true

[emitter]
# Measured by `namtor-spike emitter`. See docs/spikes/2026-08-04-m1-hardware-findings.md
#
# method values:
#   msxu             standardised Microsoft XU  -> tier 1, probed silently
#   vendor_xu        vendor-specific XU         -> tier 2, requires consent
#   always_on        energised by streaming     -> no strobe differential
#   firmware_strobed device strobes itself      -> pair from metadata, no control
#   unknown          nothing found              -> R3-FAIL for this device
#
# EXPECTED on this device: the IR sensor's MSXU (unit 4 on VideoControl
# interface 2) advertises bmControls 20 01, i.e. selectors 6 and 9. The IR
# sensor's vendor XU advertises nothing. The colour sensor's vendor XU is the
# one advertising bf — do not confuse them.
method        = "msxu"
guid          = "0f3f95dc-2632-4c4e-92c9-a04782f43bc8"
vc_interface  = 2            # IR sensor's VideoControl interface
unit          = 4
selector      = 0            # <- from spike output; try 6 and 9 first
payload_on    = ""           # <- from spike output, hex
payload_off   = ""           # <- from spike output, hex

# Fallback path if MSXU proves inert. Requires tier-2 consent (spec §4.10.1).
[emitter.fallback]
method       = "vendor_xu"
guid         = "28f03370-6311-4a2e-ba2c-6890eb334016"
vc_interface = 2
unit         = 3

[emitter.idle]
# From scripts/spike-emitter-idle.sh. Establishes what the sensor does with no
# emitter control applied, which determines what probing even means.
verdict      = "idle_dark"   # idle_dark | always_on | firmware_strobed
mean_luma    = 0.4           # of 255
alternation  = 0.01          # even/odd frame difference; >15 implies firmware strobing

[emitter.lifecycle]
# Spec §4.10.2. Enforced by the daemon, recorded here for per-device tuning.
max_on_ms = 2000

[strobe]
settle_ms    = 6
use_metadata = true

[bandwidth]
# From scripts/spike-dual-stream.sh
concurrent_ir_rgb = false    # <- set from R1 verdict

[exposure]
# From scripts/spike-ae-hunting.sh
ae_hunts = false             # <- set from R2 verdict

[meta]
verified_by = "finley"
verified_on = "2026-08-04"
kernel      = "7.1.4"
```

- [ ] **Step 4: Verify the entry parses**

Run:

```bash
python3 -c "import tomllib,sys; d=tomllib.load(open('quirks/devices/3277-0018.toml','rb')); print(d['device']['vid_pid'], d['emitter']['method'])"
```

Expected: `3277:0018 vendor_xu` (or `unknown` if R3 failed).

- [ ] **Step 5: Commit**

```bash
git add docs/spikes quirks
git commit -m "docs: M1 spike findings and quirks database entry #1"
```

---

## M1 completion criteria

- [ ] `cargo test --all` passes
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] `./scripts/check-spdx.sh` passes
- [ ] Descriptor parsing tests pass against the **real** reference fixture, proving the L8 IR detection claim (spec §4.1)
- [ ] `scripts/spike-emitter-idle.sh` has been run **first**, before any emitter probing, and its verdict recorded
- [ ] `namtor-spike modes` has been run — **before** `namtor-spike emitter` — and its Tier 0 verdict recorded
- [ ] `namtor-spike emitter` has been run against the real camera and its output recorded (skip only if Tier 0 passed)
- [ ] `scripts/spike-dual-stream.sh` has been run and its verdict recorded
- [ ] `scripts/spike-ae-hunting.sh` has been run and its verdict recorded
- [ ] `docs/spikes/2026-08-04-m1-hardware-findings.md` contains verbatim output, not summaries
- [ ] `quirks/devices/3277-0018.toml` reflects measured values
- [ ] R1, R2, R3, R9 each have an explicit verdict

**Order matters.** The idle baseline runs before emitter probing: if the device
turns out to be `always_on` or `firmware_strobed`, probing means something
different and the emitter section of the quirks entry changes shape.

## Decision point after M1

**If R3 passed:** proceed to write the M2 plan. The design holds.

**If R3 failed:** stop and revisit spec §4.2, §4.4, and §6.4 before planning M2.
Options in order of preference — (a) MSXU probing with a wider selector range
and non-boolean payloads, (b) USB control transfers via libusb bypassing
`uvcvideo`, (c) accept static-IR liveness and rescope gate G4 honestly. Do not
plan M2 against a design whose central assumption has been falsified.
