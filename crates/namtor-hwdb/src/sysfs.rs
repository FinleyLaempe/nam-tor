// SPDX-License-Identifier: Apache-2.0
//! sysfs USB device enumeration.
//!
//! Walks `/sys/bus/usb/devices/` and recovers the VID/PID/bcdDevice triple
//! the kernel exposes as separate files. Devices lacking `idVendor` (hubs,
//! root controllers) are skipped silently.

use std::fs;
use std::path::{Path, PathBuf};

use crate::HwdbError;

/// A USB device discovered under sysfs, with the path to its raw descriptor
/// blob (`/sys/.../descriptors`) that `namtor_uvc::descriptors` parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub sysfs_path: PathBuf,
    pub vid: u16,
    pub pid: u16,
    pub bcd_device: u16,
    pub descriptors_path: PathBuf,
}

/// Enumerates USB devices under `<sysfs_root>/bus/usb/devices/`.
///
/// Each entry is a symlink to a real device directory; the kernel exposes
/// `idVendor`, `idProduct`, `bcdDevice`, and `descriptors` as plain files.
/// Entries lacking `idVendor` (host controllers, hubs) are skipped. Results
/// are sorted by sysfs path for deterministic output.
pub fn enumerate_usb(sysfs_root: &Path) -> Result<Vec<UsbDevice>, HwdbError> {
    let bus_dir = sysfs_root.join("bus/usb/devices");
    let mut entries: Vec<PathBuf> = match fs::read_dir(&bus_dir) {
        Ok(it) => it.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(HwdbError::Sysfs(format!("{} not found", bus_dir.display())));
        }
        Err(e) => return Err(HwdbError::from(e)),
    };
    entries.sort();

    let mut out = Vec::new();
    for dir in entries {
        let Some(vid) = read_hex(&dir, "idVendor")? else {
            continue;
        };
        let Some(pid) = read_hex(&dir, "idProduct")? else {
            continue;
        };
        // bcdDevice is optional in practice; default to 0 when absent.
        let bcd = read_hex(&dir, "bcdDevice")?.unwrap_or(0);
        let descriptors_path = dir.join("descriptors");
        out.push(UsbDevice {
            sysfs_path: dir,
            vid,
            pid,
            bcd_device: bcd,
            descriptors_path,
        });
    }
    Ok(out)
}

fn read_hex(dir: &Path, name: &str) -> Result<Option<u16>, HwdbError> {
    let path = dir.join(name);
    let Ok(content) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    u16::from_str_radix(trimmed, 16)
        .map(Some)
        .map_err(|_| HwdbError::Sysfs(format!("malformed {name} in {}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture_blob() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/descriptors/3277-0018.bin"
        ))
        .expect("reference descriptor fixture missing")
    }

    /// Builds a fake sysfs tree:
    ///   <root>/bus/usb/devices/3-8        -> real dir with idVendor etc.
    ///   <root>/bus/usb/devices/usb1        -> hub dir without idVendor
    fn fake_sysfs(root: &Path) -> PathBuf {
        let real = root.join("real/3-8");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("idVendor"), "3277").unwrap();
        fs::write(real.join("idProduct"), "0018").unwrap();
        fs::write(real.join("bcdDevice"), "0001").unwrap();
        fs::write(real.join("descriptors"), fixture_blob()).unwrap();

        let hub = root.join("real/usb1");
        fs::create_dir_all(&hub).unwrap();
        // hub: no idVendor file

        let devs = root.join("bus/usb/devices");
        fs::create_dir_all(&devs).unwrap();
        symlink(&real, devs.join("3-8")).unwrap();
        symlink(&hub, devs.join("usb1")).unwrap();
        root.to_path_buf()
    }

    #[test]
    fn enumerates_the_reference_device_and_skips_the_hub() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fake_sysfs(tmp.path());

        let devs = enumerate_usb(&root).expect("enumeration failed");
        assert_eq!(devs.len(), 1, "hub without idVendor must be skipped");

        let d = &devs[0];
        assert_eq!(d.vid, 0x3277);
        assert_eq!(d.pid, 0x0018);
        assert_eq!(d.bcd_device, 0x0001);
        assert!(d.descriptors_path.ends_with("descriptors"));
    }

    #[test]
    fn missing_sysfs_root_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("nope");
        let err = enumerate_usb(&bogus).unwrap_err();
        assert!(matches!(err, HwdbError::Sysfs(_)));
    }
}
