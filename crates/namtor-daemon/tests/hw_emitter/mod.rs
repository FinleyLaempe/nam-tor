// SPDX-License-Identifier: GPL-3.0-only
//! Arming the reference camera's IR emitter from inside a hardware-gated
//! test, safely.
//!
//! # Why this exists
//!
//! [`enrollment_live`](../enrollment_live.rs) used to skip with "arming the
//! emitter needs ... namtor-daemon as root". That claim was wrong. The
//! quirks entry for the reference device records, as a MEASURED fact:
//!
//! ```toml
//! [access]
//! requires_root = false
//! ```
//!
//! and the seat ACL on `/dev/video2` gives the logged-in desktop user `rw`.
//! `UVCIOC_CTRL_QUERY` is an ioctl on that node, so the same user can drive
//! the MSXU mode control that arms the emitter. This module does exactly
//! that, and nothing else.
//!
//! # Safety
//!
//! Arming the emitter lights an IR LED pointed at whoever is in front of the
//! machine. `AGENTS.md`: *"Don't ship emitter control without watchdog +
//! start-up reconciliation."* Both are honoured here:
//!
//! * **Start-up reconciliation** — [`ArmedEmitter::arm`] calls
//!   [`EmitterController::force_off_before_anything`] before it writes
//!   anything, so a stale armed state from a crashed process is cleared
//!   first.
//! * **RAII off** — [`ArmedEmitter`] forces `payload_off` on `Drop`. A
//!   panicking assertion still disarms, because `Drop` runs during unwind.
//! * **Watchdog** — the caller drives a [`Watchdog`] built
//!   [`from_quirks`](Watchdog::from_quirks) (`max_on_ms = 2000` for the
//!   reference device) and calls [`ArmedEmitter::force_off`] the moment it
//!   expires. See [`watchdog_for`].
//!
//! No ioctl is hand-rolled: everything goes through the shipped
//! `namtor_camera::emitter_ctrl` / `namtor_camera::watchdog` API.

use std::path::{Path, PathBuf};

use namtor_camera::emitter_ctrl::EmitterController;
use namtor_camera::watchdog::{force_off_lib, SystemClock, Watchdog};
use namtor_hwdb::quirks::{load_quirks_dir, EmitterCfg};
use namtor_uvc::device::V4l2Device;
use namtor_uvc::probe::SafeXu;

/// The reference development device, `quirks/devices/3277-0018.toml`.
pub const REFERENCE_VID_PID: &str = "3277:0018";

/// Loads the emitter configuration for `vid_pid` from the quirks
/// directories, returning the directory that supplied it alongside the
/// config so a skip can name it.
///
/// `LayoutPaths::quirks_dirs` carries a *relative* `quirks/devices` for the
/// source layout, which only resolves when the process runs from the repo
/// root; `cargo test` runs from the crate directory. The caller therefore
/// passes absolute candidates.
pub fn emitter_cfg(dirs: &[PathBuf], vid_pid: &str) -> Option<(PathBuf, EmitterCfg)> {
    for dir in dirs {
        let Ok(db) = load_quirks_dir(dir) else {
            continue;
        };
        if let Some(entry) = db.entries.get(vid_pid) {
            return Some((dir.clone(), entry.emitter.clone()));
        }
    }
    None
}

/// A watchdog bounding how long the emitter may stay lit, taken from the
/// device's own `[emitter.lifecycle] max_on_ms` (2000 ms on the reference
/// camera). The caller must poll [`Watchdog::check_expired`] in its frame
/// loop and force the emitter off when it fires.
pub fn watchdog_for<'a>(cfg: &EmitterCfg, clock: &'a SystemClock) -> Watchdog<'a, SystemClock> {
    Watchdog::from_quirks(cfg, clock)
}

/// The emitter's `max_on_ms` budget, for logging and loop bounds.
pub fn max_on_ms(cfg: &EmitterCfg) -> u32 {
    cfg.lifecycle.as_ref().map(|l| l.max_on_ms).unwrap_or(2000)
}

/// An armed IR emitter that turns itself off again no matter how the caller
/// leaves scope — normal return, early return, or panic.
pub struct ArmedEmitter<'a> {
    xu: SafeXu<V4l2Device>,
    cfg: &'a EmitterCfg,
    armed: bool,
    /// The bytes GET_CUR read back straight after arming.
    readback: Vec<u8>,
}

impl<'a> ArmedEmitter<'a> {
    /// Forces the emitter off, then arms it, then reads the control back.
    ///
    /// On any failure the emitter is left off and the error text carries the
    /// underlying `UVCIOC_CTRL_QUERY` errno, which is the whole point when
    /// the question is whether an unprivileged user may do this at all.
    pub fn arm(node: &Path, cfg: &'a EmitterCfg) -> Result<Self, String> {
        let dev = V4l2Device::open(node)
            .map_err(|e| format!("open {} for XU access: {e}", node.display()))?;
        let mut xu = SafeXu::new(dev);

        {
            let mut ctrl = EmitterController::new(&mut xu, cfg);
            // Start-up reconciliation: clear any stale armed state first.
            ctrl.force_off_before_anything()
                .map_err(|e| format!("force_off_before_anything: {e}"))?;
            // `apply_on` re-runs the force-off internally and then writes
            // `payload_on`. Its error is the errno-bearing one.
            if let Err(e) = ctrl.apply_on() {
                return Err(format!("apply_on: {e}"));
            }
            // Hand ownership of the armed state to the guard below, so the
            // controller's own `Drop` does not disarm on this scope exit.
            ctrl.release_to_guard();
        }

        let mut guard = Self {
            xu,
            cfg,
            armed: true,
            readback: Vec::new(),
        };
        let readback = {
            let mut ctrl = EmitterController::new(&mut guard.xu, cfg);
            let r = ctrl.verify().map_err(|e| format!("verify: {e}"));
            ctrl.release_to_guard();
            r
        };
        match readback {
            Ok(bytes) if bytes == cfg.payload_on => {
                guard.readback = bytes;
                Ok(guard)
            }
            Ok(bytes) => Err(format!(
                "GET_CUR read back {bytes:02x?} but payload_on is {:02x?}; emitter not armed",
                cfg.payload_on
            )),
            Err(e) => Err(e),
        }
    }

    /// What GET_CUR returned immediately after arming.
    pub fn readback(&self) -> &[u8] {
        &self.readback
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Writes `payload_off` and disarms. Idempotent, never panics; safe to
    /// call from the watchdog path, from the happy path, and from `Drop`.
    pub fn force_off(&mut self) {
        if !self.armed {
            return;
        }
        match force_off_lib(&mut self.xu, self.cfg) {
            Ok(()) => self.armed = false,
            Err(e) => eprintln!("EMITTER FORCE-OFF FAILED: {e}"),
        }
    }

    /// Reads the control back. Used after [`force_off`](Self::force_off) to
    /// prove on the wire that the device is in `payload_off`.
    pub fn current(&mut self) -> Result<Vec<u8>, String> {
        let mut ctrl = EmitterController::new(&mut self.xu, self.cfg);
        let r = ctrl.verify().map_err(|e| format!("verify: {e}"));
        ctrl.release_to_guard();
        r
    }
}

impl Drop for ArmedEmitter<'_> {
    fn drop(&mut self) {
        self.force_off();
        // Belt and braces: even if `armed` was already false, re-read and
        // shout if the device is somehow still in the on payload. A silent
        // lit emitter is the one outcome this whole module exists to avoid.
        if let Ok(cur) = self.current() {
            if cur == self.cfg.payload_on {
                eprintln!("EMITTER STILL ARMED AFTER FORCE-OFF: {cur:02x?}");
                let _ = force_off_lib(&mut self.xu, self.cfg);
            }
        }
    }
}

/// Opens the node purely to read the emitter control, without arming
/// anything. Used to prove the emitter is off after everything is torn down.
pub fn read_control(node: &Path, cfg: &EmitterCfg) -> Result<Vec<u8>, String> {
    let dev = V4l2Device::open(node).map_err(|e| format!("open {}: {e}", node.display()))?;
    let mut xu = SafeXu::new(dev);
    let unit = cfg.unit.ok_or("emitter cfg missing unit")?;
    let selector = cfg.selector.ok_or("emitter cfg missing selector")?;
    xu.probe_control(unit, selector)
        .map(|p| p.current)
        .map_err(|e| format!("probe_control: {e}"))
}
