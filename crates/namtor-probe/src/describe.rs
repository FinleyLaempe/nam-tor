// SPDX-License-Identifier: Apache-2.0
//! `describe` subcommand: human-readable classification report for a device.

use std::path::Path;

use namtor_hwdb::pipeline::{classify, Classification};
use namtor_hwdb::quirks::QuirksDb;
use namtor_hwdb::sysfs::UsbDevice;
use namtor_uvc::descriptors::format_guid;

/// Builds the human-readable report for one USB device against a quirks db.
///
/// Surfaces the VID:PID, the IR interface, the IR format GUID, the
/// classification source, the XU units (MSXU-first with advertised selectors),
/// and the correlated `/dev/videoN` nodes. Pure: no I/O beyond what `classify`
/// already performs.
pub fn run_describe(usb: &UsbDevice, db: &QuirksDb) -> Result<String, String> {
    let c = classify(usb, db).map_err(|e| e.to_string())?;
    Ok(render(usb, &c))
}

fn render(usb: &UsbDevice, c: &Classification) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "VID:PID     {:04x}:{:04x}  (bcd {:04x})\n",
        usb.vid, usb.pid, usb.bcd_device
    ));
    out.push_str(&format!("IR interface: {}\n", c.ir_interface));
    out.push_str(&format!("IR format GUID: {}\n", c.ir_format_guid));
    if let Some(rgb) = c.rgb_interface {
        out.push_str(&format!("RGB interface: {rgb}\n"));
    } else {
        out.push_str("RGB interface: (none)\n");
    }
    out.push_str(&format!("metadata node: {}\n", c.metadata_node));
    out.push_str(&format!(
        "source: {}\n",
        match c.source {
            namtor_hwdb::pipeline::ClassSource::Quirks => "quirks",
            namtor_hwdb::pipeline::ClassSource::FormatGuid => "format_guid",
            namtor_hwdb::pipeline::ClassSource::Heuristic => "heuristic",
        }
    ));

    out.push_str("\nExtension units (MSXU first):\n");
    for xu in &c.xu_units {
        let kind = if namtor_hwdb::xu_index::is_msxu(&xu.guid) {
            "MSXU"
        } else {
            "vendor"
        };
        out.push_str(&format!(
            "  if{} unit {:>3} {} {}  selectors {:?}\n",
            xu.interface,
            xu.unit_id,
            kind,
            format_guid(&xu.guid),
            xu.advertised_selectors()
        ));
    }

    out.push_str("\nV4L2 nodes:\n");
    for n in &c.dev_nodes {
        out.push_str(&format!(
            "  {}  if{}  metadata={}  name=\"{}\"\n",
            n.dev_path.display(),
            n.interface,
            n.is_metadata,
            n.name
        ));
    }

    out
}

/// Loads the system quirks db from the given directory (default
/// `quirks/devices/`), tolerant of a missing directory.
pub fn load_quirks_db(dir: &Path) -> QuirksDb {
    match namtor_hwdb::quirks::load_quirks_dir(dir) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("warn: failed to load quirks from {}: {e}", dir.display());
            QuirksDb::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_hwdb::quirks::load_quirks_dir;
    use std::fs;
    use std::path::PathBuf;

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

    fn fake_3277(tmp: &std::path::Path) -> (UsbDevice, PathBuf) {
        // Real sysfs: the USB device dir and its interface dirs are siblings
        // under `.../bus/usb/devices/`.
        let devs = tmp.join("bus/usb/devices");
        fs::create_dir_all(&devs).unwrap();
        let usb_dir = devs.join("3-8");
        fs::create_dir_all(&usb_dir).unwrap();
        // Four V4L2 nodes across three interfaces, matching the reference camera.
        fn mk(devs: &std::path::Path, iface: &str, video: &str, name: &str) {
            let d = devs.join(iface).join("video4linux").join(video);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("name"), name).unwrap();
        }
        mk(&devs, "3-8:1.0", "video0", "Sonix USB2.0 FHD UVC WebCam");
        mk(
            &devs,
            "3-8:1.1",
            "video1",
            "Sonix USB2.0 FHD UVC WebCam Metadata",
        );
        mk(&devs, "3-8:1.3", "video2", "Sonix USB2.0 FHD UVC IR Camera");
        mk(
            &devs,
            "3-8:1.3",
            "video3",
            "Sonix USB2.0 FHD UVC IR Metadata",
        );

        let descriptors = usb_dir.join("descriptors");
        fs::write(&descriptors, fixture_blob()).unwrap();

        let usb = UsbDevice {
            sysfs_path: usb_dir,
            vid: 0x3277,
            pid: 0x0018,
            bcd_device: 0x0001,
            descriptors_path: descriptors.clone(),
        };
        (usb, descriptors)
    }

    fn write_quirks(tmp: &std::path::Path) {
        let dir = tmp.join("quirks");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("3277-0018.toml"), reference_quirks_text()).unwrap();
    }

    #[test]
    fn describes_the_reference_device_from_quirks() {
        let tmp = tempfile::tempdir().unwrap();
        let (usb, _desc) = fake_3277(tmp.path());
        write_quirks(tmp.path());
        let db = load_quirks_dir(&tmp.path().join("quirks")).unwrap();

        let report = run_describe(&usb, &db).expect("describe failed");

        assert!(report.contains("3277:0018"), "report must name the device");
        assert!(
            report.contains("IR interface: 3"),
            "IR interface must come from nodes.ir_streaming"
        );
        assert!(report.contains("MSXU"), "MSXU XUs must be labelled");
        assert!(
            report.contains("source: quirks"),
            "matched device reports source=quirks"
        );
        // XU lines list selectors 6,9 for MSXU on interface 2.
        assert!(
            report.contains("selectors [6, 9]"),
            "MSXU advertised selectors must appear; got:\n{report}"
        );
        assert!(
            report.contains("/dev/video2") && report.contains("/dev/video3"),
            "correlated nodes must be listed"
        );
    }

    #[test]
    fn describe_works_without_quirks_via_format_guid() {
        let tmp = tempfile::tempdir().unwrap();
        let (usb, _desc) = fake_3277(tmp.path());
        let db = QuirksDb::default();

        let report = run_describe(&usb, &db).expect("describe failed");
        assert!(report.contains("source: format_guid"));
        assert!(report.contains("IR interface: 3"));
    }
}
