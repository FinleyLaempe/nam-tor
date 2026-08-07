// SPDX-License-Identifier: Apache-2.0
//! USB interface → V4L2 node correlation.
//!
//! A multi-sensor UVC camera exposes several `/dev/videoN` nodes, one per
//! streaming interface, plus a metadata node per sensor. Node numbering is
//! allocation-order dependent and changes between boots, so we correlate by
//! **interface number**, not by node index. The metadata node is identified
//! by its `name` file containing "metadata" (case-insensitive).

use std::fs;
use std::path::PathBuf;

use crate::sysfs::UsbDevice;
use crate::HwdbError;

/// A `/dev/videoN` node correlated to its USB interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V4l2Node {
    pub dev_path: PathBuf,
    pub interface: u8,
    pub is_metadata: bool,
    pub name: String,
}

/// Correlate `/dev/videoN` nodes to a USB device by walking its interface
/// directories.
///
/// Real Linux sysfs exposes USB interface directories
/// (`<bus>-<port>:<cfg>.<iface>`) as **siblings** of the USB device directory
/// under `/sys/bus/usb/devices/`, not as children of the device directory. We
/// derive the device basename (e.g. `3-8`) from `usb.sysfs_path`, scan its
/// parent for siblings whose name starts with `<basename>:`, and read each
/// sibling's `video4linux/videoN` entries. The `<basename>:` prefix guard
/// prevents correlating interfaces that belong to a different device (e.g. a
/// hub child `3-8.1:1.0`, whose name does not start with `3-8:`).
///
/// The interface number is parsed from the sibling directory name. The `name`
/// file behind each `videoN` node identifies metadata nodes (case-insensitive
/// "metadata"). Output is sorted by node index for determinism.
pub fn correlate_nodes(usb: &UsbDevice) -> Result<Vec<V4l2Node>, HwdbError> {
    let mut out = Vec::new();

    let parent = usb.sysfs_path.parent().ok_or_else(|| {
        HwdbError::Sysfs(format!(
            "{} has no parent directory",
            usb.sysfs_path.display()
        ))
    })?;
    let basename = usb
        .sysfs_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| HwdbError::Sysfs(format!("{} has no basename", usb.sysfs_path.display())))?;
    let iface_prefix = format!("{basename}:");

    let entries = fs::read_dir(parent).map_err(HwdbError::from)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(&iface_prefix) {
            continue;
        }
        let Some(iface) = parse_iface_dir_name(name) else {
            continue;
        };
        let v4l_dir = path.join("video4linux");
        let Ok(nodes) = fs::read_dir(&v4l_dir) else {
            continue;
        };
        for node in nodes.flatten() {
            let node_path = node.path();
            let Some(node_name) = node_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(idx) = parse_video_index(node_name) else {
                continue;
            };
            let human = fs::read_to_string(node_path.join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let is_metadata = human.to_lowercase().contains("metadata");
            out.push(V4l2Node {
                dev_path: PathBuf::from(format!("/dev/video{idx}")),
                interface: iface,
                is_metadata,
                name: human,
            });
        }
    }

    out.sort_by_key(|n| {
        n.dev_path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("video"))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    });
    Ok(out)
}

/// Extracts the interface number from a `<bus>-<port>:<config>.<iface>` dir.
fn parse_iface_dir_name(name: &str) -> Option<u8> {
    let after_colon = name.split(':').nth(1)?;
    let iface = after_colon.split('.').nth(1)?;
    iface.parse().ok()
}

/// Extracts the numeric index from a `videoN` entry name.
fn parse_video_index(name: &str) -> Option<u32> {
    name.strip_prefix("video")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Builds an interface dir as a **sibling** of the USB device dir under a
    /// `.../devices/` parent, matching real sysfs layout.
    fn make_sibling_node(devs: &Path, iface_dir: &str, video: &str, name: &str) {
        let dir = devs.join(iface_dir).join("video4linux").join(video);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("name"), name).unwrap();
    }

    fn fake_usb(root: &Path) -> UsbDevice {
        let devs = root.join("bus/usb/devices");
        fs::create_dir_all(&devs).unwrap();
        let usb_dir = devs.join("3-8");
        fs::create_dir_all(&usb_dir).unwrap();
        // Three interface directories as siblings of 3-8: 1.0, 1.1, 1.3.
        make_sibling_node(&devs, "3-8:1.0", "video0", "Sonix USB2.0 FHD UVC WebCam");
        make_sibling_node(
            &devs,
            "3-8:1.1",
            "video1",
            "Sonix USB2.0 FHD UVC WebCam Metadata",
        );
        make_sibling_node(&devs, "3-8:1.3", "video2", "Sonix USB2.0 FHD UVC IR Camera");
        make_sibling_node(
            &devs,
            "3-8:1.3",
            "video3",
            "Sonix USB2.0 FHD UVC IR Metadata",
        );
        UsbDevice {
            sysfs_path: usb_dir.clone(),
            vid: 0x3277,
            pid: 0x0018,
            bcd_device: 0x0001,
            descriptors_path: usb_dir.join("descriptors"),
        }
    }

    #[test]
    fn correlates_four_nodes_with_metadata_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let usb = fake_usb(tmp.path());

        let nodes = correlate_nodes(&usb).expect("correlation failed");
        assert_eq!(nodes.len(), 4);

        assert_eq!(nodes[0].dev_path, PathBuf::from("/dev/video0"));
        assert_eq!(nodes[0].interface, 0);
        assert!(!nodes[0].is_metadata);

        assert_eq!(nodes[1].dev_path, PathBuf::from("/dev/video1"));
        assert_eq!(nodes[1].interface, 1);
        assert!(nodes[1].is_metadata);

        assert_eq!(nodes[2].dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(nodes[2].interface, 3);
        assert!(!nodes[2].is_metadata);

        assert_eq!(nodes[3].dev_path, PathBuf::from("/dev/video3"));
        assert_eq!(nodes[3].interface, 3);
        assert!(nodes[3].is_metadata);
    }

    #[test]
    fn ignores_directories_that_are_not_interface_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let usb = fake_usb(tmp.path());
        // A stray sibling dir that doesn't match the iface prefix.
        fs::create_dir_all(tmp.path().join("bus/usb/devices/stray")).unwrap();

        let nodes = correlate_nodes(&usb).expect("correlation failed");
        assert_eq!(nodes.len(), 4);
    }

    /// Regression: real Linux sysfs exposes USB interface directories
    /// (`<bus>-<port>:<cfg>.<iface>`) as **siblings** of the USB device
    /// directory under `/sys/bus/usb/devices/`, not as children of it. The
    /// reference camera `3-8` has its interfaces at `3-8:1.0`, `3-8:1.1`,
    /// `3-8:1.3` next to it — and a hub child device `3-8.1:1.0` must NOT be
    /// correlated to `3-8`.
    #[test]
    fn correlates_nodes_from_sibling_interface_dirs_real_sysfs_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let devs = tmp.path().join("bus/usb/devices");
        fs::create_dir_all(&devs).unwrap();
        // USB device dir is a sibling of its interface dirs.
        let usb_dir = devs.join("3-8");
        fs::create_dir_all(&usb_dir).unwrap();
        make_sibling_node(&devs, "3-8:1.0", "video0", "Sonix USB2.0 FHD UVC WebCam");
        make_sibling_node(
            &devs,
            "3-8:1.1",
            "video1",
            "Sonix USB2.0 FHD UVC WebCam Metadata",
        );
        make_sibling_node(&devs, "3-8:1.3", "video2", "Sonix USB2.0 FHD UVC IR Camera");
        make_sibling_node(
            &devs,
            "3-8:1.3",
            "video3",
            "Sonix USB2.0 FHD UVC IR Metadata",
        );
        // A distinct USB device on a hub port of 3-8 — must NOT be correlated,
        // even though its interface dir parses as `<bus>-<port>:<cfg>.<iface>`.
        make_sibling_node(&devs, "3-8.1:1.0", "video4", "Other Camera");
        // An unrelated hub entry with no idVendor.
        fs::create_dir_all(devs.join("usb1")).unwrap();

        let usb = UsbDevice {
            sysfs_path: usb_dir.clone(),
            vid: 0x3277,
            pid: 0x0018,
            bcd_device: 0x0001,
            descriptors_path: usb_dir.join("descriptors"),
        };

        let nodes = correlate_nodes(&usb).expect("correlation failed");
        assert_eq!(nodes.len(), 4, "only 3-8:* interfaces, not 3-8.1:* or hubs");
        assert_eq!(nodes[2].dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(nodes[2].interface, 3);
        assert!(!nodes[2].is_metadata);
        assert_eq!(nodes[3].dev_path, PathBuf::from("/dev/video3"));
        assert_eq!(nodes[3].interface, 3);
        assert!(nodes[3].is_metadata);
    }
}
