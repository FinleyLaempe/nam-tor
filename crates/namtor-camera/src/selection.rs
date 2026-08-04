// SPDX-License-Identifier: GPL-3.0-only
//! Deterministic multi-camera selection.
//!
//! Spec §4.10.4: when more than one IR-capable camera is present, the daemon
//! must select one deterministically. Order of preference:
//!
//! 1. **Configured** — an admin-set `device = /dev/videoN` in the config.
//! 2. **Enrolled** — the device the user enrolled against.
//! 3. **Internal** — a non-removable device (sysfs `removable == "fixed"`).
//! 4. **First IR** — the first classified IR node by index.
//!
//! Ties within a tier break by lowest `/dev/videoN` index for determinism.

use std::path::Path;

use namtor_hwdb::nodes::V4l2Node;

/// Why a camera was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectReason {
    Configured,
    Enrolled,
    Internal,
    FirstIr,
}

/// The chosen device and the reason it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedDevice {
    pub node: V4l2Node,
    pub reason: SelectReason,
}

/// Selects one camera from `candidates` per the preference order.
///
/// `configured` and `enrolled` are `/dev/videoN` paths (e.g. `"/dev/video2"`).
/// `internal` detection reads the `removable` sysfs attribute of the parent
/// USB device; here it is approximated by a `is_internal` closure so the
/// function is pure and testable without sysfs.
pub fn select_camera(
    candidates: &[V4l2Node],
    configured: Option<&str>,
    enrolled: Option<&str>,
    is_internal: impl Fn(&V4l2Node) -> bool,
) -> Option<SelectedDevice> {
    if candidates.is_empty() {
        return None;
    }

    let by_path =
        |p: &str| -> Option<&V4l2Node> { candidates.iter().find(|n| n.dev_path == Path::new(p)) };

    if let Some(p) = configured {
        if let Some(n) = by_path(p) {
            return Some(SelectedDevice {
                node: n.clone(),
                reason: SelectReason::Configured,
            });
        }
    }

    if let Some(p) = enrolled {
        if let Some(n) = by_path(p) {
            return Some(SelectedDevice {
                node: n.clone(),
                reason: SelectReason::Enrolled,
            });
        }
    }

    let internal: Vec<&V4l2Node> = candidates.iter().filter(|n| is_internal(n)).collect();
    if let Some(n) = pick_lowest_index(&internal) {
        return Some(SelectedDevice {
            node: n.clone(),
            reason: SelectReason::Internal,
        });
    }

    let all: Vec<&V4l2Node> = candidates.iter().collect();
    pick_lowest_index(&all).map(|n| SelectedDevice {
        node: n.clone(),
        reason: SelectReason::FirstIr,
    })
}

fn pick_lowest_index<'a>(nodes: &'a [&'a V4l2Node]) -> Option<&'a V4l2Node> {
    nodes.iter().min_by_key(|n| node_index(n)).copied()
}

fn node_index(n: &V4l2Node) -> u32 {
    n.dev_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("video"))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn node(dev: &str, iface: u8) -> V4l2Node {
        V4l2Node {
            dev_path: PathBuf::from(dev),
            interface: iface,
            is_metadata: false,
            name: format!("cam {dev}"),
        }
    }

    #[test]
    fn empty_candidates_returns_none() {
        assert!(select_camera(&[], None, None, |_| false).is_none());
    }

    #[test]
    fn configured_takes_priority_over_internal_and_first() {
        let cands = vec![
            node("/dev/video2", 3),
            node("/dev/video4", 3),
            node("/dev/video6", 3),
        ];
        let sel = select_camera(&cands, Some("/dev/video6"), None, |_| true).unwrap();
        assert_eq!(sel.node.dev_path, PathBuf::from("/dev/video6"));
        assert_eq!(sel.reason, SelectReason::Configured);
    }

    #[test]
    fn enrolled_takes_priority_over_internal_when_not_configured() {
        let cands = vec![node("/dev/video2", 3), node("/dev/video4", 3)];
        let sel = select_camera(&cands, None, Some("/dev/video4"), |_| true).unwrap();
        assert_eq!(sel.node.dev_path, PathBuf::from("/dev/video4"));
        assert_eq!(sel.reason, SelectReason::Enrolled);
    }

    #[test]
    fn internal_takes_priority_over_first_ir() {
        let cands = vec![node("/dev/video4", 3), node("/dev/video2", 3)];
        let sel = select_camera(&cands, None, None, |n| {
            n.dev_path.as_path() == Path::new("/dev/video4")
        })
        .unwrap();
        assert_eq!(sel.node.dev_path, PathBuf::from("/dev/video4"));
        assert_eq!(sel.reason, SelectReason::Internal);
    }

    #[test]
    fn first_ir_picks_lowest_index_when_no_internal() {
        let cands = vec![
            node("/dev/video4", 3),
            node("/dev/video2", 3),
            node("/dev/video6", 3),
        ];
        let sel = select_camera(&cands, None, None, |_| false).unwrap();
        assert_eq!(sel.node.dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(sel.reason, SelectReason::FirstIr);
    }

    #[test]
    fn configured_missing_falls_through_to_enrolled() {
        let cands = vec![node("/dev/video2", 3)];
        let sel =
            select_camera(&cands, Some("/dev/video99"), Some("/dev/video2"), |_| false).unwrap();
        assert_eq!(sel.reason, SelectReason::Enrolled);
    }

    #[test]
    fn internal_tie_breaks_by_lowest_index() {
        let cands = vec![node("/dev/video6", 3), node("/dev/video2", 3)];
        let sel = select_camera(&cands, None, None, |_| true).unwrap();
        assert_eq!(sel.node.dev_path, PathBuf::from("/dev/video2"));
        assert_eq!(sel.reason, SelectReason::Internal);
    }
}
