// SPDX-License-Identifier: Apache-2.0
//! UVC extension unit control queries.

use std::os::raw::c_void;

/// Mirrors the kernel's `struct uvc_xu_control_query` from
/// `include/uapi/linux/uvcvideo.h`.
#[repr(C)]
#[derive(Debug)]
pub struct UvcXuControlQuery {
    pub unit: u8,
    pub selector: u8,
    pub query: u8,
    pub size: u16,
    pub data: *mut c_void,
}

/// Video Class-Specific Request Codes, `include/uapi/linux/usb/video.h` A.8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Query {
    SetCur = 0x01,
    GetCur = 0x81,
    GetMin = 0x82,
    GetMax = 0x83,
    GetRes = 0x84,
    GetLen = 0x85,
    GetInfo = 0x86,
    GetDef = 0x87,
}

impl Query {
    /// True for queries that modify device state.
    ///
    /// Used by the safe wrapper to enforce the probe safety rules: a write is
    /// only permitted after a successful read of the same control.
    pub fn is_write(self) -> bool {
        matches!(self, Query::SetCur)
    }
}

// _IOWR('u', 0x21, struct uvc_xu_control_query)
nix::ioctl_readwrite!(uvc_ctrl_query, b'u', 0x21, UvcXuControlQuery);

/// Decoded GET_INFO capability byte, UVC 1.5 §4.1.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlInfo(u8);

impl CtrlInfo {
    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn supports_get(self) -> bool {
        self.0 & 0b0000_0001 != 0
    }

    /// Probe safety rule 4: controls without SET support are never written.
    pub fn supports_set(self) -> bool {
        self.0 & 0b0000_0010 != 0
    }

    pub fn is_disabled(self) -> bool {
        self.0 & 0b0000_0100 != 0
    }

    pub fn is_autoupdate(self) -> bool {
        self.0 & 0b0000_1000 != 0
    }

    pub fn is_async(self) -> bool {
        self.0 & 0b0001_0000 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn query_struct_matches_kernel_abi() {
        // struct uvc_xu_control_query {
        //     __u8 unit; __u8 selector; __u8 query;
        //     __u16 size; __u8 __user *data;
        // };
        // On LP64: u8 u8 u8 pad, u16 at 4, pad, pointer at 8. Total 16.
        assert_eq!(size_of::<UvcXuControlQuery>(), 16);
        assert_eq!(align_of::<UvcXuControlQuery>(), 8);
    }

    #[test]
    fn query_codes_match_linux_usb_video_h() {
        assert_eq!(Query::SetCur as u8, 0x01);
        assert_eq!(Query::GetCur as u8, 0x81);
        assert_eq!(Query::GetMin as u8, 0x82);
        assert_eq!(Query::GetMax as u8, 0x83);
        assert_eq!(Query::GetRes as u8, 0x84);
        assert_eq!(Query::GetLen as u8, 0x85);
        assert_eq!(Query::GetInfo as u8, 0x86);
        assert_eq!(Query::GetDef as u8, 0x87);
    }

    #[test]
    fn set_cur_is_the_only_write_query() {
        assert!(Query::SetCur.is_write());
        for q in [
            Query::GetCur,
            Query::GetMin,
            Query::GetMax,
            Query::GetRes,
            Query::GetLen,
            Query::GetInfo,
            Query::GetDef,
        ] {
            assert!(!q.is_write(), "{q:?} must not be classified as a write");
        }
    }

    #[test]
    fn ctrl_info_decodes_get_and_set_support() {
        // UVC 1.5 §4.1.2: bit 0 = GET supported, bit 1 = SET supported.
        let both = CtrlInfo::from_bits(0b0000_0011);
        assert!(both.supports_get());
        assert!(both.supports_set());

        let read_only = CtrlInfo::from_bits(0b0000_0001);
        assert!(read_only.supports_get());
        assert!(!read_only.supports_set());
    }

    #[test]
    fn ctrl_info_decodes_state_bits() {
        // bit 2 = disabled due to automatic mode, bit 3 = autoupdate,
        // bit 4 = asynchronous.
        assert!(CtrlInfo::from_bits(0b0000_0100).is_disabled());
        assert!(CtrlInfo::from_bits(0b0000_1000).is_autoupdate());
        assert!(CtrlInfo::from_bits(0b0001_0000).is_async());
    }

    #[test]
    fn a_control_with_no_set_support_is_never_probe_writable() {
        // Probe safety rule 4.
        let info = CtrlInfo::from_bits(0b0000_0001);
        assert!(!info.supports_set());
    }
}
