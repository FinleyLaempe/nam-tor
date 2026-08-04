// SPDX-License-Identifier: Apache-2.0
//! USB descriptor parsing.
//!
//! `uvcvideo` collapses the IR sensor's format GUID to V4L2 `GREY` and
//! discards the GUID. `GREY` means greyscale, not infrared — which is why
//! every other Linux tool guesses. Reading the raw descriptors from sysfs
//! recovers the same signal Windows Hello keys on.

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
        g[10..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
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
                VC_EXTENSION_UNIT if len >= 22 => {
                    // bLength, bDescriptorType, bDescriptorSubtype, bUnitID,
                    // guidExtensionCode[16], bNumControls, bNrInPins,
                    // baSourceID[bNrInPins], bControlSize, bmControls[..],
                    // iExtension
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
                VS_FORMAT_UNCOMPRESSED if len >= 21 => {
                    // bLength, bDescriptorType, bDescriptorSubtype,
                    // bFormatIndex, bNumFrameDescriptors, guidFormat[16], ...
                    let mut guid = [0u8; 16];
                    guid.copy_from_slice(&desc[5..21]);
                    set.formats.push(FormatDescriptor {
                        interface: current_interface,
                        format_index: desc[3],
                        guid,
                    });
                }
                _ => {}
            }
        }

        i += len;
    }

    set
}

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
            .find(|xu| xu.interface == 2 && format_guid(&xu.guid).starts_with("28f03370-6311-4a2e"))
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
            .find(|xu| xu.interface == 0 && format_guid(&xu.guid).starts_with("28f03370-6311-4a2e"))
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
            .find(|xu| xu.interface == 2 && format_guid(&xu.guid).starts_with("0f3f95dc-2632-4c4e"))
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
            .find(|xu| xu.interface == 0 && format_guid(&xu.guid).starts_with("0f3f95dc-2632-4c4e"))
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
