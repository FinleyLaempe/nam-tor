// SPDX-License-Identifier: Apache-2.0
//! Extension-unit enumeration, MSXU first.
//!
//! A multi-sensor camera declares one set of extension units per VideoControl
//! interface. Probing must be filtered to a single interface, and the
//! Microsoft standardised extension unit (MSXU) is always probed before any
//! vendor unit — MSXU is the tier-0 silent path (spec §4.3, G1.4).

use namtor_uvc::descriptors::{DescriptorSet, ExtensionUnit};

/// The MSXU GUID prefix: `0f3f95dc-2632-4c4e-...`.
///
/// Stored as the raw on-the-wire bytes (little-endian dword, little-endian
/// 16-bit groups) so it can be compared directly against `ExtensionUnit::guid`.
const MSXU_PREFIX: [u8; 8] = [0xdc, 0x95, 0x3f, 0x0f, 0x32, 0x26, 0x4e, 0x4c];

/// True when a GUID carries the Microsoft extension-unit prefix.
pub fn is_msxu(guid: &[u8; 16]) -> bool {
    guid[..8] == MSXU_PREFIX
}

/// Returns the extension units declared on `interface`, MSXU entries first.
///
/// The sort is stable, so within each class (MSXU vs vendor) the declaration
/// order from the descriptor blob is preserved.
pub fn enumerate_xus(set: &DescriptorSet, interface: u8) -> Vec<ExtensionUnit> {
    let mut units: Vec<ExtensionUnit> = set
        .extension_units
        .iter()
        .filter(|xu| xu.interface == interface)
        .cloned()
        .collect();
    // MSXU (true) -> key 0 (first); vendor (false) -> key 1.
    units.sort_by_key(|xu| !is_msxu(&xu.guid) as u8);
    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_uvc::descriptors::{format_guid, parse_descriptors};

    fn reference_blob() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/descriptors/3277-0018.bin"
        ))
        .expect("reference descriptor fixture missing")
    }

    #[test]
    fn enumerates_msxu_before_vendor_on_ir_interface() {
        let set = parse_descriptors(&reference_blob());
        let units = enumerate_xus(&set, 2);
        assert_eq!(units.len(), 2);
        assert!(is_msxu(&units[0].guid), "MSXU must come first");
        assert_eq!(units[0].unit_id, 4);
        assert!(format_guid(&units[0].guid).starts_with("0f3f95dc-2632-4c4e"));
        assert!(!is_msxu(&units[1].guid));
        assert_eq!(units[1].unit_id, 3);
        assert!(format_guid(&units[1].guid).starts_with("28f03370-6311-4a2e"));
    }

    #[test]
    fn enumerates_msxu_before_vendor_on_colour_interface() {
        let set = parse_descriptors(&reference_blob());
        let units = enumerate_xus(&set, 0);
        assert_eq!(units.len(), 2);
        assert!(is_msxu(&units[0].guid), "MSXU first on every interface");
        assert!(!is_msxu(&units[1].guid));
    }

    #[test]
    fn unknown_interface_returns_empty() {
        let set = parse_descriptors(&reference_blob());
        assert!(enumerate_xus(&set, 99).is_empty());
    }
}
