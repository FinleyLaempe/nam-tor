// SPDX-License-Identifier: Apache-2.0
//! Format-GUID classification: recovers the IR/depth/colour signal that
//! `uvcvideo` discards when it collapses every uncompressed format to V4L2
//! `GREY`.
//!
//! Microsoft encodes the format identifier in the leading dword of the GUID
//! (see [`namtor_uvc::descriptors::guid_leading_dword`]). `L8` (0x32) is IR;
//! `D16` (0x40) is depth. Known colour FourCCs are recognised explicitly;
//! anything else is `Unknown` — the heuristic pipeline (Task 7) picks up
//! lone greyscale `Unknown` interfaces as IR candidates.

use namtor_uvc::descriptors::{format_guid, guid_leading_dword, is_l8_ir_format, DescriptorSet};

/// Coarse format class recovered from the descriptor GUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    /// 8-bit luminance — `MFVideoFormat_L8`, leading dword 0x32. An IR sensor.
    Ir,
    /// 16-bit depth, leading dword 0x40.
    Depth,
    /// A recognised colour FourCC (YUY2, UYVY, NV12, ...).
    Colour,
    /// Unrecognised — neither a known colour FourCC nor L8/D16. A lone
    /// greyscale interface here is a heuristic IR candidate.
    Unknown,
}

/// A descriptor format paired with its classified kind and textual GUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedFormat {
    pub interface: u8,
    pub format_index: u8,
    pub guid: [u8; 16],
    pub kind: FormatKind,
    pub guid_str: String,
}

/// Classifies every format in a descriptor set.
pub fn classify_formats(set: &DescriptorSet) -> Vec<ClassifiedFormat> {
    set.formats
        .iter()
        .map(|f| ClassifiedFormat {
            interface: f.interface,
            format_index: f.format_index,
            guid: f.guid,
            kind: classify_guid(&f.guid),
            guid_str: format_guid(&f.guid),
        })
        .collect()
}

/// Classifies a single format GUID.
pub fn classify_guid(guid: &[u8; 16]) -> FormatKind {
    if is_l8_ir_format(guid) {
        return FormatKind::Ir;
    }
    let dword = guid_leading_dword(guid);
    if dword == 0x40 {
        return FormatKind::Depth;
    }
    if is_colour_fourcc(dword) {
        return FormatKind::Colour;
    }
    FormatKind::Unknown
}

/// Recognised colour FourCCs encoded as the leading dword of a UVC format
/// GUID. Kept deliberately small — the reference fixture only emits YUY2, and
/// the heuristic pipeline treats unrecognised FourCCs as `Unknown` rather than
/// misclassifying them as colour.
fn is_colour_fourcc(dword: u32) -> bool {
    matches!(
        dword,
        0x3259_5559 // YUY2 / YUYV
            | 0x5956_5955 // UYVY
            | 0x3231_564e // NV12
            | 0x3231_5659 // YV12
            | 0x3032_3449 // I420
            | 0x3032_344d // M420
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_uvc::descriptors::parse_descriptors;

    fn reference_blob() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/descriptors/3277-0018.bin"
        ))
        .expect("reference descriptor fixture missing")
    }

    #[test]
    fn classifies_the_reference_fixture() {
        let set = parse_descriptors(&reference_blob());
        let cf = classify_formats(&set);

        let ir = cf
            .iter()
            .find(|c| c.kind == FormatKind::Ir)
            .expect("no IR format classified");
        assert_eq!(ir.interface, 3, "IR sits on the IR streaming interface");
        assert!(
            ir.guid_str.starts_with("00000032-0002-"),
            "IR GUID text starts with the L8 leading dword: {}",
            ir.guid_str
        );

        let colour = cf
            .iter()
            .find(|c| c.kind == FormatKind::Colour)
            .expect("no colour format classified");
        assert_eq!(
            colour.interface, 1,
            "colour sits on the RGB streaming interface"
        );

        assert!(
            cf.iter().all(|c| c.kind != FormatKind::Depth),
            "reference device declares no depth format"
        );
    }

    #[test]
    fn l8_guid_is_ir_and_d16_is_depth() {
        let mut ir = [0u8; 16];
        ir[0] = 0x32;
        assert_eq!(classify_guid(&ir), FormatKind::Ir);

        let mut depth = [0u8; 16];
        depth[0] = 0x40;
        assert_eq!(classify_guid(&depth), FormatKind::Depth);
    }

    #[test]
    fn unrecognised_guid_is_unknown_not_colour() {
        // A greyscale format that is neither L8 nor a known colour FourCC.
        // The heuristic pipeline (Task 7) treats this as an IR candidate.
        let mut grey = [0u8; 16];
        grey[0] = 0x50;
        grey[1] = 0x50;
        grey[2] = 0x50;
        grey[3] = 0x50;
        assert_eq!(classify_guid(&grey), FormatKind::Unknown);
    }
}
