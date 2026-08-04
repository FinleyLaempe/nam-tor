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
}
