// SPDX-License-Identifier: GPL-3.0-only
//! The record `namtor emitter-off` needs to darken the IR emitter after the
//! daemon is gone.
//!
//! `ExecStopPost` runs after SIGKILL, when the daemon can no longer be asked
//! anything. It used to be spelled:
//!
//! ```text
//! ExecStopPost=/usr/bin/namtor emitter-off /dev/video2
//! ```
//!
//! with the quirks file resolved from `NAMTOR_QUIRKS` or, failing that, a
//! hardcoded path to the *reference* device's entry. Both halves were wrong
//! anywhere but the development machine. `namtor_hwdb::nodes` says why in its
//! own module docs: "Node numbering is allocation-order dependent and changes
//! between boots". A backstop that is only correct when the IR camera happens
//! to land on `/dev/video2`, and that otherwise writes the reference device's
//! payload to whatever camera did, is not a backstop.
//!
//! So the daemon writes this record before it ever turns the emitter on, and
//! `emitter-off` reads it. It holds the device's **stable USB identity** —
//! vendor, product, and the UVC interface number — and the three fields
//! `force_off_lib` actually reads. The `/dev/videoN` path is recorded too, but
//! only as a hint: [`EmitterState::resolve_device`] re-derives the node from
//! sysfs and falls back to the recorded path only when that fails.
//!
//! It lives under `/var/lib/namtor` because that is the one directory the unit
//! grants the daemon (`ReadWritePaths=/var/lib/namtor`); `/run/namtor` is
//! `root:namtor-auth 0750` and the daemon, which only ever receives the socket
//! as a passed fd, is not in that group. Being persistent rather than tmpfs is
//! precisely why the node is re-resolved instead of trusted.

use std::fs;
use std::path::{Path, PathBuf};

/// Default location of the record. Inside `ReadWritePaths=/var/lib/namtor`.
pub const DEFAULT_PATH: &str = "/var/lib/namtor/emitter-state.toml";

/// Everything needed to turn one machine's IR emitter off without the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitterState {
    /// USB vendor id of the camera whose emitter was armed.
    pub vid: u16,
    /// USB product id.
    pub pid: u16,
    /// UVC interface number of the streaming node. A multi-sensor camera
    /// exposes several `/dev/videoN` nodes; this picks the right one after a
    /// renumber, which a bare node path cannot.
    pub interface: u8,
    /// The node as it was at arm time. A hint only — see `resolve_device`.
    pub device: PathBuf,
    /// XU unit id.
    pub unit: u8,
    /// XU control selector.
    pub selector: u8,
    /// The bytes that darken the emitter.
    pub payload_off: Vec<u8>,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !t.len().is_multiple_of(2) {
        return Err(format!(
            "payload_off has an odd number of hex digits: {t:?}"
        ));
    }
    (0..t.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&t[i..i + 2], 16)
                .map_err(|e| format!("payload_off is not hex at byte {}: {e}", i / 2))
        })
        .collect()
}

impl EmitterState {
    /// Render as TOML. Hand-rolled rather than derived: `EmitterCfg` is
    /// `Deserialize`-only and its payloads are hex strings, and this file is
    /// read on the crash path where fewer moving parts is the point.
    pub fn to_toml(&self) -> String {
        format!(
            "# Written by the Nam-tor daemon before arming the IR emitter.\n\
             # Read by `namtor emitter-off` (systemd ExecStopPost) to darken the\n\
             # emitter after the daemon is gone, including after SIGKILL.\n\
             # The device node is re-resolved from vid/pid/interface; the path\n\
             # below is only a fallback, because node numbering changes between\n\
             # boots.\n\
             vid = {}\n\
             pid = {}\n\
             interface = {}\n\
             device = \"{}\"\n\
             unit = {}\n\
             selector = {}\n\
             payload_off = \"{}\"\n",
            self.vid,
            self.pid,
            self.interface,
            self.device.display(),
            self.unit,
            self.selector,
            hex_encode(&self.payload_off),
        )
    }

    /// Parse the TOML written by [`Self::to_toml`].
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let v: toml::Value = toml::from_str(text).map_err(|e| format!("parse: {e}"))?;
        let int = |k: &str| -> Result<i64, String> {
            v.get(k)
                .and_then(|x| x.as_integer())
                .ok_or_else(|| format!("missing or non-integer field {k:?}"))
        };
        let vid = u16::try_from(int("vid")?).map_err(|_| "vid out of range".to_string())?;
        let pid = u16::try_from(int("pid")?).map_err(|_| "pid out of range".to_string())?;
        let interface =
            u8::try_from(int("interface")?).map_err(|_| "interface out of range".to_string())?;
        let unit = u8::try_from(int("unit")?).map_err(|_| "unit out of range".to_string())?;
        let selector =
            u8::try_from(int("selector")?).map_err(|_| "selector out of range".to_string())?;
        let device = v
            .get("device")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "missing or non-string field \"device\"".to_string())?;
        let payload_off = v
            .get("payload_off")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "missing or non-string field \"payload_off\"".to_string())?;
        let payload_off = hex_decode(payload_off)?;
        if payload_off.is_empty() {
            return Err("payload_off is empty; there is nothing to write".into());
        }
        Ok(Self {
            vid,
            pid,
            interface,
            device: PathBuf::from(device),
            unit,
            selector,
            payload_off,
        })
    }

    /// Write atomically: a torn record on the crash path would be worse than
    /// none, because it reads as a parse error rather than as "no emitter".
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, self.to_toml()).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, path).map_err(|e| format!("rename into {}: {e}", path.display()))?;
        Ok(())
    }

    /// Read the record. `Ok(None)` means the file is absent, which is the
    /// ordinary case on a machine whose emitter has never been armed and must
    /// not be reported as a failure.
    pub fn read(path: &Path) -> Result<Option<Self>, String> {
        match fs::read_to_string(path) {
            Ok(t) => Self::from_toml(&t).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    /// The node to write to now, re-derived from sysfs by vid/pid/interface.
    ///
    /// Falls back to the recorded path when sysfs cannot answer — an
    /// out-of-date node is still a better guess than refusing to act, and
    /// `force_off_lib` verifies the XU control exists before writing anything,
    /// so aiming at the wrong device is a no-op rather than a stray write.
    pub fn resolve_device(&self, sysfs_root: &Path) -> PathBuf {
        self.resolve_device_opt(sysfs_root)
            .unwrap_or_else(|| self.device.clone())
    }

    /// `Some` only when sysfs positively identifies the node.
    pub fn resolve_device_opt(&self, sysfs_root: &Path) -> Option<PathBuf> {
        let devices = namtor_hwdb::sysfs::enumerate_usb(sysfs_root).ok()?;
        let usb = devices
            .iter()
            .find(|d| d.vid == self.vid && d.pid == self.pid)?;
        let nodes = namtor_hwdb::nodes::correlate_nodes(usb).ok()?;
        nodes
            .iter()
            .find(|n| n.interface == self.interface && !n.is_metadata)
            .map(|n| n.dev_path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EmitterState {
        EmitterState {
            vid: 0x3277,
            pid: 0x0018,
            interface: 2,
            device: PathBuf::from("/dev/video2"),
            unit: 4,
            selector: 6,
            payload_off: vec![0x01, 0x03, 0x01, 0x00, 0x00],
        }
    }

    #[test]
    fn toml_round_trips() {
        let s = sample();
        let back = EmitterState::from_toml(&s.to_toml()).expect("parse");
        assert_eq!(back, s);
    }

    #[test]
    fn payload_off_round_trips_as_hex() {
        let s = sample();
        assert!(
            s.to_toml().contains("payload_off = \"0103010000\""),
            "payload should be lower-case hex, got:\n{}",
            s.to_toml()
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/emitter-state.toml");
        let s = sample();
        s.write(&path).expect("write");
        let back = EmitterState::read(&path).expect("read").expect("present");
        assert_eq!(back, s);
    }

    #[test]
    fn write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("emitter-state.toml");
        sample().write(&path).unwrap();
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file left behind: {strays:?}");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let got = EmitterState::read(&dir.path().join("absent.toml")).expect("not an error");
        assert!(got.is_none(), "an absent record means no emitter was armed");
    }

    #[test]
    fn a_corrupt_file_is_an_error_not_a_silent_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("emitter-state.toml");
        std::fs::write(&path, "vid = 1\nthis is not toml").unwrap();
        assert!(
            EmitterState::read(&path).is_err(),
            "a corrupt record must be reported, not treated as 'nothing to turn off'"
        );
    }

    #[test]
    fn an_empty_payload_is_rejected() {
        let mut t = sample().to_toml();
        t = t.replace("payload_off = \"0103010000\"", "payload_off = \"\"");
        assert!(EmitterState::from_toml(&t).is_err());
    }

    #[test]
    fn odd_hex_is_rejected() {
        let t = sample()
            .to_toml()
            .replace("payload_off = \"0103010000\"", "payload_off = \"010\"");
        assert!(EmitterState::from_toml(&t).is_err());
    }

    #[test]
    fn resolve_falls_back_to_the_recorded_node_when_sysfs_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        // An empty sysfs root: enumerate_usb finds no bus directory.
        let s = sample();
        assert_eq!(s.resolve_device(dir.path()), PathBuf::from("/dev/video2"));
        assert_eq!(s.resolve_device_opt(dir.path()), None);
    }
}

#[cfg(test)]
mod unit_wiring_tests {
    use std::path::Path;

    fn unit_text() -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/systemd/namtor-daemon.service");
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
    }

    /// The SIGKILL backstop must not name a `/dev/videoN` node.
    ///
    /// `ExecStopPost=/usr/bin/namtor emitter-off /dev/video2` was correct only
    /// where the IR camera happened to be video2. `namtor_hwdb::nodes` says in
    /// its own module docs that node numbering "changes between boots", so the
    /// hardcoded form aimed one device's payload at whatever camera landed on
    /// that node.
    #[test]
    fn exec_stop_post_does_not_hardcode_a_device_node() {
        let text = unit_text();
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with("ExecStopPost="))
            .expect("the unit must keep an ExecStopPost emitter backstop");
        assert!(
            !line.contains("/dev/video"),
            "ExecStopPost hardcodes a video node, which is not stable across \
             boots: {line}"
        );
        assert!(
            line.contains("emitter-off"),
            "ExecStopPost must still force the emitter off: {line}"
        );
    }

    /// The backstop must not depend on NAMTOR_QUIRKS, which the unit never set
    /// — leaving `emitter-off` to fall back to the reference device's entry.
    #[test]
    fn the_backstop_does_not_depend_on_a_quirks_env_var() {
        // Directives only. The comment above ExecStopPost explains what the
        // old NAMTOR_QUIRKS fallback got wrong, and a whole-file grep would
        // fail on that explanation — a test that forbids its own rationale
        // from being written down.
        let text = unit_text();
        let directives: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect();
        assert!(
            !directives.iter().any(|l| l.contains("NAMTOR_QUIRKS")),
            "the crash path must carry its own payload, not re-read quirks: {directives:?}"
        );
    }

    /// The daemon can only write where the unit lets it.
    #[test]
    fn the_state_path_is_inside_a_writable_path() {
        let text = unit_text();
        let writable: Vec<&str> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix("ReadWritePaths="))
            .flat_map(|v| v.split_whitespace())
            .collect();
        assert!(
            writable.iter().any(|w| super::DEFAULT_PATH.starts_with(w)),
            "{} is not under any ReadWritePaths= entry {:?}; the daemon could \
             not write the record and the backstop would have nothing to read",
            super::DEFAULT_PATH,
            writable
        );
    }
}
