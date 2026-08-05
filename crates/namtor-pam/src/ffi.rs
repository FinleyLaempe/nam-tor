// SPDX-License-Identifier: GPL-3.0-only
//! Minimal FFI bindings to libpam (Linux-PAM).
//!
//! Only the symbols and types the PAM module needs: `pam_handle_t`, the item
//! codes (`PAM_USER`, `PAM_RHOST`, `PAM_TTY`, `PAM_SERVICE`), the return codes
//! (`PAM_SUCCESS`, `PAM_AUTHINFO_UNAVAIL`, …), and `pam_get_item` / `pam_get_user`.
//!
//! Spec §3.4: no dynamic linking beyond libc and libpam. We link `libpam`
//! via `#[link]`; the crate is a `cdylib` producing `pam_namtor.so`.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

/// Opaque PAM handle.
#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

pub const PAM_SERVICE: c_int = 1;
pub const PAM_USER: c_int = 2;
pub const PAM_TTY: c_int = 3;
pub const PAM_RHOST: c_int = 4;
pub const PAM_AUTHTOK: c_int = 6;

pub const PAM_SUCCESS: c_int = 0;
pub const PAM_SERVICE_ERR: c_int = 3;
pub const PAM_SYSTEM_ERR: c_int = 4;
pub const PAM_AUTH_ERR: c_int = 7;
pub const PAM_AUTHINFO_UNAVAIL: c_int = 9;
pub const PAM_USER_UNKNOWN: c_int = 10;
pub const PAM_MAXTRIES: c_int = 11;

#[link(name = "pam")]
extern "C" {
    pub fn pam_get_item(pamh: *mut PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;

    pub fn pam_get_user(
        pamh: *mut PamHandle,
        user: *mut *const c_char,
        prompt: *const c_char,
    ) -> c_int;
}

/// Fetch a PAM string item. `Ok(None)` = unset, `Err(code)` = `pam_get_item` failure.
///
/// # Safety
/// `pamh` must be a valid PAM handle from the framework.
pub unsafe fn get_item_string(
    pamh: *mut PamHandle,
    item_type: c_int,
) -> Result<Option<String>, c_int> {
    let mut raw: *const c_void = ptr::null();
    let rc = pam_get_item(pamh, item_type, &mut raw);
    if rc != PAM_SUCCESS {
        return Err(rc);
    }
    if raw.is_null() {
        return Ok(None);
    }
    let cstr = CStr::from_ptr(raw as *const c_char);
    Ok(Some(cstr.to_string_lossy().into_owned()))
}

/// Fetch the target username.
///
/// # Safety
/// `pamh` must be a valid PAM handle.
pub unsafe fn get_user(pamh: *mut PamHandle) -> Result<String, c_int> {
    let mut raw: *const c_char = ptr::null();
    let rc = pam_get_user(pamh, &mut raw, ptr::null());
    if rc != PAM_SUCCESS {
        return Err(rc);
    }
    if raw.is_null() {
        return Err(PAM_SYSTEM_ERR);
    }
    let cstr = CStr::from_ptr(raw);
    Ok(cstr.to_string_lossy().into_owned())
}

/// Resolve a Unix username to a numeric uid via `getpwnam_r`.
pub fn uid_of_user(name: &str) -> Option<u32> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let mut buf = vec![0u8; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = ptr::null_mut();
    let rc = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut pwd,
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    Some(pwd.pw_uid)
}

pub fn pam_code_name(code: c_int) -> &'static str {
    match code {
        PAM_SUCCESS => "PAM_SUCCESS",
        PAM_SERVICE_ERR => "PAM_SERVICE_ERR",
        PAM_SYSTEM_ERR => "PAM_SYSTEM_ERR",
        PAM_AUTH_ERR => "PAM_AUTH_ERR",
        PAM_AUTHINFO_UNAVAIL => "PAM_AUTHINFO_UNAVAIL",
        PAM_USER_UNKNOWN => "PAM_USER_UNKNOWN",
        PAM_MAXTRIES => "PAM_MAXTRIES",
        _ => "PAM_OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_of_root_is_zero() {
        assert_eq!(uid_of_user("root"), Some(0));
    }

    #[test]
    fn uid_of_nonexistent_user_is_none() {
        assert_eq!(uid_of_user("namtor_definitely_no_such_user_xyz"), None);
    }

    #[test]
    fn pam_code_names_match_constants() {
        assert_eq!(pam_code_name(PAM_SUCCESS), "PAM_SUCCESS");
        assert_eq!(pam_code_name(PAM_AUTHINFO_UNAVAIL), "PAM_AUTHINFO_UNAVAIL");
        assert_eq!(pam_code_name(9999), "PAM_OTHER");
    }

    #[test]
    fn item_constants_match_linux_pam() {
        assert_eq!(PAM_SERVICE, 1);
        assert_eq!(PAM_USER, 2);
        assert_eq!(PAM_TTY, 3);
        assert_eq!(PAM_RHOST, 4);
        assert_eq!(PAM_AUTHTOK, 6);
    }

    #[test]
    fn return_code_constants_match_linux_pam() {
        assert_eq!(PAM_SUCCESS, 0);
        assert_eq!(PAM_AUTH_ERR, 7);
        assert_eq!(PAM_AUTHINFO_UNAVAIL, 9);
        assert_eq!(PAM_MAXTRIES, 11);
    }
}
