// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-camera-emitter-off`: best-effort force-off invoked by systemd's
//! `ExecStopPost=` on unexpected teardown. Thin wrapper over
//! [`namtor_camera::watchdog::force_off_on_device`].

use std::path::PathBuf;
use std::process::ExitCode;

use namtor_camera::watchdog::force_off_on_device;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        eprintln!("usage: namtor-camera-emitter-off <device>");
        return ExitCode::from(2);
    }
    let device = PathBuf::from(&args[0]);

    // Load the quirks entry for this device. In the systemd unit, the daemon
    // writes the matched entry to a known path before starting; this binary
    // reads it. For now we require the path via an env var, defaulting to
    // the repo's reference entry.
    let quirks_path = std::env::var("NAMTOR_QUIRKS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("quirks/devices/3277-0018.toml"));

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
