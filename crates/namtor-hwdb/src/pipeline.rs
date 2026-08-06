// SPDX-License-Identifier: Apache-2.0
//! End-to-end classification pipeline: quirks → format-GUID → heuristic.
//!
//! For a discovered USB device, produce the IR interface, IR format GUID,
//! optional RGB interface, metadata flag, and the XU list to probe. Three
//! sources in priority order:
//!
//! 1. **Quirks** — a matched entry in the database is authoritative.
//! 2. **FormatGuid** — the L8 format GUID recovered from the raw descriptors.
//! 3. **Heuristic** — a lone greyscale non-colour interface, when neither of
//!    the above is available. Always reported so unknown devices still produce
//!    a usable classification rather than a silent failure.

use std::fs;

use namtor_uvc::descriptors::{parse_descriptors, DescriptorSet, ExtensionUnit};

use crate::classify::{classify_formats, FormatKind};
use crate::nodes::{correlate_nodes, V4l2Node};
use crate::quirks::{QuirksDb, QuirksEntry};
use crate::sysfs::UsbDevice;
use crate::HwdbError;

/// How the IR interface was identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassSource {
    Quirks,
    FormatGuid,
    Heuristic,
}

/// The result of classifying a USB device.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub ir_interface: u8,
    pub ir_format_guid: String,
    pub rgb_interface: Option<u8>,
    pub metadata_node: bool,
    pub source: ClassSource,
    pub xu_units: Vec<ExtensionUnit>,
    pub dev_nodes: Vec<V4l2Node>,
}

/// The IR, optional metadata, and optional RGB V4L2 nodes extracted from a
/// [`Classification`]'s `dev_nodes` by interface-number pairing.
///
/// Node numbering is allocation-order dependent; pairing by USB interface
/// number is the stable, deterministic correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePairing {
    pub ir: V4l2Node,
    pub metadata: Option<V4l2Node>,
    pub rgb: Option<V4l2Node>,
}

impl Classification {
    /// Pairs the IR streaming node, its metadata node (if the classification
    /// reports one), and the RGB streaming node (if classified) from
    /// `dev_nodes` by interface number.
    ///
    /// Returns `None` when no non-metadata node on `ir_interface` is found —
    /// the device was classified but the correlated `/dev/videoN` nodes do not
    /// include the IR streaming interface (a torn topology).
    pub fn pair(&self) -> Option<NodePairing> {
        let ir = self
            .dev_nodes
            .iter()
            .find(|n| n.interface == self.ir_interface && !n.is_metadata)?;

        let metadata = if self.metadata_node {
            self.dev_nodes
                .iter()
                .find(|n| n.interface == self.ir_interface && n.is_metadata)
                .cloned()
        } else {
            None
        };

        let rgb = self.rgb_interface.and_then(|rgb_if| {
            self.dev_nodes
                .iter()
                .find(|n| n.interface == rgb_if && !n.is_metadata)
                .cloned()
        });

        Some(NodePairing {
            ir: ir.clone(),
            metadata,
            rgb,
        })
    }
}

/// Classifies a device by reading its raw descriptors from sysfs and applying
/// the quirks → format-GUID → heuristic pipeline.
pub fn classify(usb: &UsbDevice, db: &QuirksDb) -> Result<Classification, HwdbError> {
    let blob = fs::read(&usb.descriptors_path).map_err(HwdbError::from)?;
    let set = parse_descriptors(&blob);
    classify_with_set(&set, usb, db)
}

/// Classification pipeline operating on a pre-parsed descriptor set.
///
/// Exposed so tests can feed constructed [`DescriptorSet`]s directly rather
/// than hand-crafting binary descriptor blobs (the parser is already tested
/// by `namtor-uvc`).
pub fn classify_with_set(
    set: &DescriptorSet,
    usb: &UsbDevice,
    db: &QuirksDb,
) -> Result<Classification, HwdbError> {
    let dev_nodes = correlate_nodes(usb)?;

    // 1. Quirks — authoritative. Try bcd-qualified first, then unqualified.
    if let Some(entry) = db
        .match_device(usb.vid, usb.pid, Some(usb.bcd_device))
        .or_else(|| db.match_device(usb.vid, usb.pid, None))
    {
        return Ok(classification_from_quirks(entry, set, dev_nodes));
    }

    // 2. FormatGuid — L8 format recovered from the descriptors.
    let classified = classify_formats(set);
    if let Some(ir) = classified.iter().find(|c| c.kind == FormatKind::Ir) {
        let xu_units = enumerate_xus(set, ir.interface);
        return Ok(Classification {
            ir_interface: ir.interface,
            ir_format_guid: ir.guid_str.clone(),
            rgb_interface: None,
            metadata_node: false,
            source: ClassSource::FormatGuid,
            xu_units,
            dev_nodes,
        });
    }

    // 3. Heuristic — a lone greyscale non-colour interface.
    if let Some(ir) = classified.iter().find(|c| c.kind == FormatKind::Unknown) {
        let xu_units = enumerate_xus(set, ir.interface);
        return Ok(Classification {
            ir_interface: ir.interface,
            ir_format_guid: ir.guid_str.clone(),
            rgb_interface: None,
            metadata_node: false,
            source: ClassSource::Heuristic,
            xu_units,
            dev_nodes,
        });
    }

    Err(HwdbError::NotFound(format!(
        "no IR interface identified for {:04x}:{:04x}",
        usb.vid, usb.pid
    )))
}

/// Builds a Classification from a matched quirks entry. XUs are enumerated on
/// the VideoControl interface (`ir_vc_interface`), where extension units live;
/// `ir_interface` reports the VideoStreaming interface (`ir_streaming`).
fn classification_from_quirks(
    entry: &QuirksEntry,
    set: &DescriptorSet,
    dev_nodes: Vec<V4l2Node>,
) -> Classification {
    let xu_units = enumerate_xus(set, entry.nodes.ir_vc_interface);
    Classification {
        ir_interface: entry.nodes.ir_streaming,
        ir_format_guid: entry.nodes.ir_format_guid.clone(),
        rgb_interface: entry.nodes.rgb_streaming,
        metadata_node: entry.nodes.metadata_node,
        source: ClassSource::Quirks,
        xu_units,
        dev_nodes,
    }
}

fn enumerate_xus(set: &DescriptorSet, interface: u8) -> Vec<ExtensionUnit> {
    crate::xu_index::enumerate_xus(set, interface)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quirks::load_quirks_dir;
    use namtor_uvc::descriptors::{format_guid, FormatDescriptor};

    fn fixture_blob() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/descriptors/3277-0018.bin"
        ))
        .expect("reference descriptor fixture missing")
    }

    fn reference_quirks_text() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../quirks/devices/3277-0018.toml"
        ))
        .expect("reference quirks file missing")
    }

    fn write_fixture(tmp: &std::path::Path) -> std::path::PathBuf {
        let path = tmp.join("descriptors");
        std::fs::write(&path, fixture_blob()).unwrap();
        path
    }

    fn write_quirks(tmp: &std::path::Path) {
        let dir = tmp.join("quirks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("3277-0018.toml"), reference_quirks_text()).unwrap();
    }

    fn usb_at(tmp: &std::path::Path, descriptors_path: std::path::PathBuf) -> UsbDevice {
        UsbDevice {
            sysfs_path: tmp.to_path_buf(),
            vid: 0x3277,
            pid: 0x0018,
            bcd_device: 0x0001,
            descriptors_path,
        }
    }

    #[test]
    fn quirks_source_is_authoritative_for_the_reference_device() {
        let tmp = tempfile::tempdir().unwrap();
        let descriptors = write_fixture(tmp.path());
        write_quirks(tmp.path());
        let db = load_quirks_dir(&tmp.path().join("quirks")).unwrap();

        let c = classify(&usb_at(tmp.path(), descriptors), &db).unwrap();
        assert_eq!(c.source, ClassSource::Quirks);
        assert_eq!(c.ir_interface, 3, "ir_interface from nodes.ir_streaming");
        assert!(c.ir_format_guid.starts_with("00000032-"));
        assert_eq!(c.rgb_interface, Some(1));
        assert!(c.metadata_node);
        assert!(!c.xu_units.is_empty(), "XUs enumerated on ir_vc_interface");
        assert!(crate::xu_index::is_msxu(&c.xu_units[0].guid), "MSXU first");
    }

    #[test]
    fn format_guid_source_when_no_quirks_match() {
        let tmp = tempfile::tempdir().unwrap();
        let descriptors = write_fixture(tmp.path());
        let db = QuirksDb::default();

        let c = classify(&usb_at(tmp.path(), descriptors), &db).unwrap();
        assert_eq!(c.source, ClassSource::FormatGuid);
        assert_eq!(c.ir_interface, 3);
        assert!(c.ir_format_guid.starts_with("00000032-"));
    }

    #[test]
    fn heuristic_source_picks_lone_greyscale_non_colour_interface() {
        let tmp = tempfile::tempdir().unwrap();
        // YUY2 (colour) on interface 0; an unrecognised greyscale GUID on 1.
        let mut yuyv = [0u8; 16];
        // YUY2 leading dword 0x32595559, little-endian on the wire.
        yuyv[..4].copy_from_slice(&[0x59, 0x55, 0x59, 0x32]);
        let mut grey = [0u8; 16];
        grey[0] = 0x50; // neither L8 (0x32), D16 (0x40), nor a colour FourCC
        let set = DescriptorSet {
            formats: vec![
                FormatDescriptor {
                    interface: 0,
                    format_index: 1,
                    guid: yuyv,
                },
                FormatDescriptor {
                    interface: 1,
                    format_index: 1,
                    guid: grey,
                },
            ],
            extension_units: vec![],
        };
        let usb = UsbDevice {
            sysfs_path: tmp.path().to_path_buf(),
            vid: 0x9999,
            pid: 0x9999,
            bcd_device: 0,
            descriptors_path: std::path::PathBuf::from("/nonexistent"),
        };
        let db = QuirksDb::default();

        let c = classify_with_set(&set, &usb, &db).unwrap();
        assert_eq!(c.source, ClassSource::Heuristic);
        assert_eq!(
            c.ir_interface, 1,
            "picks the non-colour greyscale interface"
        );
        assert_eq!(c.ir_format_guid, format_guid(&grey));
    }

    #[test]
    fn no_ir_candidate_is_a_not_found_error() {
        let tmp = tempfile::tempdir().unwrap();
        let set = DescriptorSet::default();
        let usb = UsbDevice {
            sysfs_path: tmp.path().to_path_buf(),
            vid: 0x9999,
            pid: 0x9999,
            bcd_device: 0,
            descriptors_path: std::path::PathBuf::from("/nonexistent"),
        };
        let db = QuirksDb::default();

        let err = classify_with_set(&set, &usb, &db).unwrap_err();
        assert!(matches!(err, HwdbError::NotFound(_)));
    }

    fn mk_node(dev: &str, iface: u8, is_meta: bool) -> V4l2Node {
        V4l2Node {
            dev_path: std::path::PathBuf::from(dev),
            interface: iface,
            is_metadata: is_meta,
            name: format!("node {dev}"),
        }
    }

    fn classification_with_nodes(
        ir_iface: u8,
        rgb_iface: Option<u8>,
        metadata: bool,
        nodes: Vec<V4l2Node>,
    ) -> Classification {
        Classification {
            ir_interface: ir_iface,
            ir_format_guid: "00000032-0002-0010-8000-00aa00389b71".into(),
            rgb_interface: rgb_iface,
            metadata_node: metadata,
            source: ClassSource::Quirks,
            xu_units: vec![],
            dev_nodes: nodes,
        }
    }

    #[test]
    fn pair_extracts_ir_metadata_and_rgb_for_reference_topology() {
        let nodes = vec![
            mk_node("/dev/video0", 1, false),
            mk_node("/dev/video1", 1, true),
            mk_node("/dev/video2", 3, false),
            mk_node("/dev/video3", 3, true),
        ];
        let c = classification_with_nodes(3, Some(1), true, nodes);

        let p = c.pair().expect("IR node must be found");
        assert_eq!(p.ir.dev_path, std::path::PathBuf::from("/dev/video2"));
        assert_eq!(p.ir.interface, 3);
        assert!(!p.ir.is_metadata);
        assert_eq!(
            p.metadata.as_ref().unwrap().dev_path,
            std::path::PathBuf::from("/dev/video3")
        );
        assert!(p.metadata.as_ref().unwrap().is_metadata);
        assert_eq!(
            p.rgb.as_ref().unwrap().dev_path,
            std::path::PathBuf::from("/dev/video0")
        );
        assert!(!p.rgb.as_ref().unwrap().is_metadata);
    }

    #[test]
    fn pair_works_with_renumbered_nodes() {
        let nodes = vec![
            mk_node("/dev/video4", 3, false),
            mk_node("/dev/video5", 3, true),
            mk_node("/dev/video6", 1, false),
            mk_node("/dev/video7", 1, true),
        ];
        let c = classification_with_nodes(3, Some(1), true, nodes);

        let p = c.pair().expect("IR node found by interface, not by index");
        assert_eq!(p.ir.dev_path, std::path::PathBuf::from("/dev/video4"));
        assert_eq!(
            p.metadata.as_ref().unwrap().dev_path,
            std::path::PathBuf::from("/dev/video5")
        );
        assert_eq!(
            p.rgb.as_ref().unwrap().dev_path,
            std::path::PathBuf::from("/dev/video6")
        );
    }

    #[test]
    fn pair_returns_none_when_ir_interface_missing_from_nodes() {
        let nodes = vec![mk_node("/dev/video0", 1, false)];
        let c = classification_with_nodes(3, None, false, nodes);
        assert!(c.pair().is_none());
    }

    #[test]
    fn pair_omits_metadata_when_classification_says_none() {
        let nodes = vec![mk_node("/dev/video2", 3, false)];
        let c = classification_with_nodes(3, None, false, nodes);
        let p = c.pair().unwrap();
        assert!(p.metadata.is_none());
        assert!(p.rgb.is_none());
    }

    #[test]
    fn pair_omits_rgb_when_not_classified() {
        let nodes = vec![
            mk_node("/dev/video2", 3, false),
            mk_node("/dev/video3", 3, true),
        ];
        let c = classification_with_nodes(3, None, true, nodes);
        let p = c.pair().unwrap();
        assert!(p.rgb.is_none());
        assert!(p.metadata.is_some());
    }
}
