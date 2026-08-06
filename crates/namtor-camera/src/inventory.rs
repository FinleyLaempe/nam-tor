// SPDX-License-Identifier: GPL-3.0-only
//! Camera inventory seam: discovers IR-capable cameras and pairs their
//! IR/metadata/RGB V4L2 nodes, so the daemon can select one without hardcoded
//! device paths.
//!
//! The [`CameraInventory`] trait abstracts enumeration so the selection logic
//! is testable without real hardware. [`SysfsInventory`] is the production
//! implementation — it walks sysfs, classifies each USB device via
//! `namtor-hwdb`, and pairs nodes by interface number. [`FakeInventory`] is
//! the test double.

use std::path::{Path, PathBuf};

use namtor_hwdb::nodes::V4l2Node;
use namtor_hwdb::pipeline::classify;
use namtor_hwdb::quirks::{load_quirks_dir, QuirksDb, QuirksEntry};
use namtor_hwdb::sysfs::{enumerate_usb, UsbDevice};

use crate::selection::{select_camera, SelectReason, SelectedDevice};

/// A discovered IR-capable camera with its paired V4L2 nodes and resolved
/// quirks entry (if any).
#[derive(Debug, Clone)]
pub struct DiscoveredCamera {
    pub ir_node: V4l2Node,
    pub metadata_node: Option<V4l2Node>,
    pub rgb_node: Option<V4l2Node>,
    pub usb: UsbDevice,
    pub quirks_entry: Option<QuirksEntry>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub is_internal: bool,
}

/// The selected camera plus the reason it was chosen.
#[derive(Debug, Clone)]
pub struct SelectedCamera {
    pub camera: DiscoveredCamera,
    pub reason: SelectReason,
}

/// Abstracts V4L2 device enumeration so selection is testable without sysfs.
pub trait CameraInventory {
    fn enumerate(&self) -> Result<Vec<DiscoveredCamera>, String>;
}

/// Production inventory: walks sysfs, classifies each USB device, and pairs
/// IR/metadata/RGB nodes by interface number.
pub struct SysfsInventory {
    sysfs_root: PathBuf,
    quirks_dirs: Vec<PathBuf>,
}

impl Default for SysfsInventory {
    fn default() -> Self {
        Self {
            sysfs_root: PathBuf::from("/sys"),
            quirks_dirs: vec![
                PathBuf::from("/usr/share/namtor/quirks/devices"),
                PathBuf::from("quirks/devices"),
            ],
        }
    }
}

impl SysfsInventory {
    pub fn new(sysfs_root: PathBuf, quirks_dirs: Vec<PathBuf>) -> Self {
        Self {
            sysfs_root,
            quirks_dirs,
        }
    }
}

impl CameraInventory for SysfsInventory {
    fn enumerate(&self) -> Result<Vec<DiscoveredCamera>, String> {
        let db = load_merged_quirks(&self.quirks_dirs);
        let usb_devices = enumerate_usb(&self.sysfs_root).map_err(|e| e.to_string())?;

        let mut cameras = Vec::new();
        for usb in &usb_devices {
            let classification = match classify(usb, &db) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let Some(pairing) = classification.pair() else {
                continue;
            };

            let quirks_entry = db
                .match_device(usb.vid, usb.pid, Some(usb.bcd_device))
                .or_else(|| db.match_device(usb.vid, usb.pid, None))
                .cloned();

            let (width, height, fps) = resolution_from_quirks(&quirks_entry);
            let is_internal = read_removable(&usb.sysfs_path);

            cameras.push(DiscoveredCamera {
                ir_node: pairing.ir,
                metadata_node: pairing.metadata,
                rgb_node: pairing.rgb,
                usb: usb.clone(),
                quirks_entry,
                width,
                height,
                fps,
                is_internal,
            });
        }
        Ok(cameras)
    }
}

/// Test inventory that returns a pre-built list.
pub struct FakeInventory {
    cameras: Vec<DiscoveredCamera>,
}

impl FakeInventory {
    pub fn new(cameras: Vec<DiscoveredCamera>) -> Self {
        Self { cameras }
    }
}

impl CameraInventory for FakeInventory {
    fn enumerate(&self) -> Result<Vec<DiscoveredCamera>, String> {
        Ok(self.cameras.clone())
    }
}

/// Selects one camera from a discovered inventory using the same preference
/// order as [`select_camera`]: configured → enrolled → internal → first IR.
pub fn select_camera_from_inventory(
    cameras: &[DiscoveredCamera],
    configured: Option<&str>,
    enrolled: Option<&str>,
) -> Option<SelectedCamera> {
    let ir_nodes: Vec<V4l2Node> = cameras.iter().map(|c| c.ir_node.clone()).collect();
    let is_internal = |n: &V4l2Node| {
        cameras
            .iter()
            .any(|c| c.ir_node.dev_path == n.dev_path && c.is_internal)
    };

    let sel: SelectedDevice = select_camera(&ir_nodes, configured, enrolled, is_internal)?;
    let camera = cameras
        .iter()
        .find(|c| c.ir_node.dev_path == sel.node.dev_path)?;
    Some(SelectedCamera {
        camera: camera.clone(),
        reason: sel.reason,
    })
}

fn load_merged_quirks(dirs: &[PathBuf]) -> QuirksDb {
    let mut db = QuirksDb::default();
    for dir in dirs {
        if let Ok(partial) = load_quirks_dir(dir) {
            for (key, entry) in partial.entries {
                db.entries.insert(key, entry);
            }
        }
    }
    db
}

fn resolution_from_quirks(entry: &Option<QuirksEntry>) -> (u32, u32, u32) {
    let default = (640, 360, 30);
    let Some(entry) = entry else {
        return default;
    };
    let Some(res) = &entry.nodes.ir_resolution else {
        return default;
    };
    parse_resolution(res).unwrap_or(default)
}

fn parse_resolution(s: &str) -> Option<(u32, u32, u32)> {
    let (res, fps) = s.split_once('@')?;
    let (w, h) = res.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?, fps.parse().ok()?))
}

fn read_removable(sysfs_path: &Path) -> bool {
    std::fs::read_to_string(sysfs_path.join("removable"))
        .ok()
        .map(|s| s.trim() == "fixed")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mk_usb(vid: u16, pid: u16) -> UsbDevice {
        UsbDevice {
            sysfs_path: PathBuf::from(format!("/sys/bus/usb/devices/test-{vid:04x}")),
            vid,
            pid,
            bcd_device: 0x0001,
            descriptors_path: PathBuf::from("/nonexistent"),
        }
    }

    fn mk_node(dev: &str, iface: u8, is_meta: bool) -> V4l2Node {
        V4l2Node {
            dev_path: PathBuf::from(dev),
            interface: iface,
            is_metadata: is_meta,
            name: format!("node {dev}"),
        }
    }

    fn mk_camera(
        dev: &str,
        is_internal: bool,
        usb: UsbDevice,
        meta: Option<&str>,
        rgb: Option<&str>,
    ) -> DiscoveredCamera {
        DiscoveredCamera {
            ir_node: mk_node(dev, 3, false),
            metadata_node: meta.map(|m| mk_node(m, 3, true)),
            rgb_node: rgb.map(|r| mk_node(r, 1, false)),
            usb,
            quirks_entry: None,
            width: 640,
            height: 360,
            fps: 30,
            is_internal,
        }
    }

    #[test]
    fn empty_inventory_returns_none() {
        let cameras = vec![];
        assert!(select_camera_from_inventory(&cameras, None, None).is_none());
    }

    #[test]
    fn zero_config_reference_sonix_is_selected() {
        let usb = mk_usb(0x3277, 0x0018);
        let cam = mk_camera(
            "/dev/video2",
            true,
            usb,
            Some("/dev/video3"),
            Some("/dev/video0"),
        );
        let cameras = vec![cam];

        let sel = select_camera_from_inventory(&cameras, None, None).unwrap();
        assert_eq!(sel.camera.ir_node.dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(sel.reason, SelectReason::Internal);
        assert_eq!(
            sel.camera.metadata_node.as_ref().unwrap().dev_path,
            PathBuf::from("/dev/video3")
        );
        assert_eq!(
            sel.camera.rgb_node.as_ref().unwrap().dev_path,
            PathBuf::from("/dev/video0")
        );
    }

    #[test]
    fn fake_renumber_selects_by_interface_not_hardcoded_path() {
        let usb = mk_usb(0x3277, 0x0018);
        let cam = mk_camera(
            "/dev/video4",
            true,
            usb,
            Some("/dev/video5"),
            Some("/dev/video6"),
        );
        let cameras = vec![cam];

        let sel = select_camera_from_inventory(&cameras, None, None).unwrap();
        assert_eq!(sel.camera.ir_node.dev_path, PathBuf::from("/dev/video4"));
        assert_eq!(
            sel.camera.metadata_node.as_ref().unwrap().dev_path,
            PathBuf::from("/dev/video5")
        );
    }

    #[test]
    fn multi_camera_picks_internal_without_config() {
        let ext_usb = mk_usb(0x1111, 0x2222);
        let int_usb = mk_usb(0x3277, 0x0018);
        let external = mk_camera("/dev/video0", false, ext_usb, None, None);
        let internal = mk_camera("/dev/video2", true, int_usb, Some("/dev/video3"), None);
        let cameras = vec![external, internal];

        let sel = select_camera_from_inventory(&cameras, None, None).unwrap();
        assert_eq!(sel.camera.ir_node.dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(sel.reason, SelectReason::Internal);
    }

    #[test]
    fn multi_camera_configured_overrides_internal() {
        let ext_usb = mk_usb(0x1111, 0x2222);
        let int_usb = mk_usb(0x3277, 0x0018);
        let external = mk_camera("/dev/video0", false, ext_usb, None, None);
        let internal = mk_camera("/dev/video2", true, int_usb, Some("/dev/video3"), None);
        let cameras = vec![external.clone(), internal];

        let sel = select_camera_from_inventory(&cameras, Some("/dev/video0"), None).unwrap();
        assert_eq!(sel.camera.ir_node.dev_path, PathBuf::from("/dev/video0"));
        assert_eq!(sel.reason, SelectReason::Configured);
    }

    #[test]
    fn multi_camera_enrolled_overrides_internal() {
        let ext_usb = mk_usb(0x1111, 0x2222);
        let int_usb = mk_usb(0x3277, 0x0018);
        let external = mk_camera("/dev/video0", false, ext_usb, None, None);
        let internal = mk_camera("/dev/video2", true, int_usb, Some("/dev/video3"), None);
        let cameras = vec![external, internal];

        let sel = select_camera_from_inventory(&cameras, None, Some("/dev/video0")).unwrap();
        assert_eq!(sel.camera.ir_node.dev_path, PathBuf::from("/dev/video0"));
        assert_eq!(sel.reason, SelectReason::Enrolled);
    }

    #[test]
    fn multi_camera_no_internal_picks_lowest_index() {
        let usb_a = mk_usb(0x1111, 0x2222);
        let usb_b = mk_usb(0x3333, 0x4444);
        let cam_a = mk_camera("/dev/video4", false, usb_a, None, None);
        let cam_b = mk_camera("/dev/video2", false, usb_b, None, None);
        let cameras = vec![cam_a, cam_b];

        let sel = select_camera_from_inventory(&cameras, None, None).unwrap();
        assert_eq!(sel.camera.ir_node.dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(sel.reason, SelectReason::FirstIr);
    }

    #[test]
    fn missing_device_returns_none_actionably() {
        let cameras = vec![];
        let result = select_camera_from_inventory(&cameras, None, None);
        assert!(result.is_none(), "no IR camera must be None, not a panic");
    }

    #[test]
    fn fake_inventory_returns_injected_cameras() {
        let usb = mk_usb(0x3277, 0x0018);
        let cam = mk_camera("/dev/video2", true, usb, Some("/dev/video3"), None);
        let inv = FakeInventory::new(vec![cam]);

        let cameras = inv.enumerate().unwrap();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras[0].ir_node.dev_path, PathBuf::from("/dev/video2"));
    }

    #[test]
    fn parse_resolution_handles_known_shapes() {
        assert_eq!(parse_resolution("640x360@30"), Some((640, 360, 30)));
        assert_eq!(parse_resolution("1280x720@60"), Some((1280, 720, 60)));
        assert!(parse_resolution("bad").is_none());
        assert!(parse_resolution("640x360").is_none());
    }

    #[test]
    fn resolution_defaults_when_no_quirks() {
        let entry = None;
        assert_eq!(resolution_from_quirks(&entry), (640, 360, 30));
    }
}
