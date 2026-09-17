// SPDX-License-Identifier: GPL-3.0-only
//! `namtor emitter-off`: best-effort force-off invoked by systemd's
//! `ExecStopPost=` on unexpected teardown, including after SIGKILL.
//!
//! With no argument it reads [`crate::emitter_state`], the record the daemon
//! writes before it arms the emitter, and re-resolves the device node from the
//! recorded USB identity. That is the form the unit uses. The old
//! `<device>` form is kept for operators and tests, and still resolves its
//! payload from a quirks file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::emitter_state::{self, EmitterState};
use crate::watchdog::force_off_on_device;

pub const USAGE: &str = "\
Usage: namtor emitter-off [<device>]

Best-effort IR emitter force-off, invoked by systemd's ExecStopPost= on
unexpected teardown.

With no <device>, reads the record the daemon writes before arming the
emitter and re-resolves the node from the recorded USB vendor/product and
interface, because /dev/videoN numbering changes between boots. Exits 0 when
no record exists: nothing was ever armed, so there is nothing to turn off.

Options:
  --state <path>   Override the record path (default
                   /var/lib/namtor/emitter-state.toml)
  -h, --help       Print this help and exit
  -V, --version    Print the version and exit";

/// Entry point for `namtor emitter-off`. `args` are the arguments *after* the
/// subcommand name.
pub fn main(args: &[String]) -> ExitCode {
    // `handle_known` leaves the <device> positional alone.
    if let Some(code) = namtor_layout::cli_preamble::handle_known(
        "namtor emitter-off",
        env!("CARGO_PKG_VERSION"),
        USAGE,
        args,
    ) {
        return ExitCode::from(code);
    }
    // Split off an optional --state override before looking at positionals.
    let mut state_path = PathBuf::from(emitter_state::DEFAULT_PATH);
    let mut positional: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--state" {
            match args.get(i + 1) {
                Some(v) => {
                    state_path = PathBuf::from(v);
                    i += 2;
                }
                None => {
                    eprintln!("error: --state needs a path");
                    return ExitCode::from(2);
                }
            }
        } else {
            positional.push(&args[i]);
            i += 1;
        }
    }

    if positional.is_empty() {
        return from_state(&state_path);
    }
    if positional.len() != 1 {
        eprintln!("usage: namtor emitter-off [<device>]");
        return ExitCode::from(2);
    }
    let device = PathBuf::from(positional[0]);

    // Load the quirks entry for this device. In the systemd unit, the daemon
    // writes the matched entry to a known path before starting; this binary
    // reads it. For now we require the path via an env var, defaulting to
    // the repo's reference entry.
    let quirks_path = std::env::var("NAMTOR_QUIRKS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/usr/share/namtor/quirks/devices/3277-0018.toml"));

    let text = match std::fs::read_to_string(&quirks_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: read {}: {e}", quirks_path.display());
            return ExitCode::FAILURE;
        }
    };
    let entry: namtor_hwdb::quirks::QuirksEntry = match toml::from_str(&text) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: parse quirks: {e}");
            return ExitCode::FAILURE;
        }
    };

    match force_off_on_device(&device, &entry.emitter) {
        Ok(()) => {
            eprintln!("emitter forced off on {}", device.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Force the emitter off using the daemon's record.
///
/// An absent record exits 0. `ExecStopPost` runs on every stop, including the
/// ordinary ones on machines with no IR camera at all, and a backstop that
/// logs a failure every time the service stops trains operators to ignore it.
/// A record that exists but cannot be parsed is a real failure and is
/// reported, because something did arm the emitter.
fn from_state(state_path: &Path) -> ExitCode {
    let state = match EmitterState::read(state_path) {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!(
                "no emitter record at {} — nothing was armed, nothing to turn off",
                state_path.display()
            );
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let sysfs_root = std::env::var("NAMTOR_SYSFS_ROOT").unwrap_or_else(|_| "/sys".to_string());
    let device = state.resolve_device(Path::new(&sysfs_root));
    if device != state.device {
        eprintln!(
            "device renumbered since the emitter was armed: {} -> {} \
             (matched usb {:04x}:{:04x} interface {})",
            state.device.display(),
            device.display(),
            state.vid,
            state.pid,
            state.interface,
        );
    }

    use namtor_uvc::device::V4l2Device;
    use namtor_uvc::probe::SafeXu;
    let dev = match V4l2Device::open(&device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: open {}: {e}", device.display());
            return ExitCode::FAILURE;
        }
    };
    let mut xu = SafeXu::new(dev);
    match force_off_lib_raw(&mut xu, state.unit, state.selector, &state.payload_off) {
        Ok(()) => {
            eprintln!("emitter forced off on {}", device.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `force_off_lib` without an `EmitterCfg`. The record deliberately stores the
/// three fields that function reads rather than a whole quirks entry, so the
/// crash path never depends on a quirks file still being readable.
fn force_off_lib_raw<T: namtor_uvc::probe::XuTransport>(
    xu: &mut namtor_uvc::probe::SafeXu<T>,
    unit: u8,
    selector: u8,
    payload_off: &[u8],
) -> Result<(), String> {
    if xu.probe_control(unit, selector).is_err() {
        // The control is not there: this is not the camera we armed, or it is
        // gone. Writing anyway would be a stray write to an unknown device.
        return Ok(());
    }
    match xu.try_write(unit, selector, payload_off) {
        Ok(restorer) => {
            restorer.keep();
            Ok(())
        }
        Err(e) => Err(format!("force_off write failed: {e}")),
    }
}
