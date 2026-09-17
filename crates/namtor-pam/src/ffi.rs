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

// --- PAM conversation -------------------------------------------------------

/// `PAM_CONV` item: the application's conversation callback.
pub const PAM_CONV: c_int = 5;
/// Message styles we use. `PAM_ERROR_MSG` is rendered as an error by the
/// application; `PAM_TEXT_INFO` as ordinary text.
pub const PAM_ERROR_MSG: c_int = 3;
pub const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
pub struct PamMessage {
    pub msg_style: c_int,
    pub msg: *const std::ffi::c_char,
}

#[repr(C)]
pub struct PamResponse {
    pub resp: *mut std::ffi::c_char,
    pub resp_retcode: c_int,
}

#[repr(C)]
pub struct PamConv {
    pub conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    pub appdata_ptr: *mut c_void,
}

extern "C" {
    fn free(p: *mut c_void);
}

/// Show `text` to the user through the application's conversation function.
///
/// Best-effort and deliberately infallible: this runs on the authentication
/// path of `sudo` and the display manager, so every way it can go wrong —
/// no conversation item, a null callback, a string with an interior NUL, a
/// conversation that returns an error — results in showing nothing rather
/// than disturbing the result. The caller's verdict is never affected.
///
/// # Safety
/// `pamh` must be the live handle PAM passed to the module.
pub unsafe fn send_user_message(pamh: *mut PamHandle, style: c_int, text: &str) {
    if pamh.is_null() {
        return;
    }
    let Ok(c_text) = std::ffi::CString::new(text) else {
        return;
    };

    let mut raw: *const c_void = std::ptr::null();
    // SAFETY: `pamh` is live; PAM_CONV yields a `struct pam_conv *`.
    let rc = unsafe { pam_get_item(pamh, PAM_CONV, &mut raw) };
    if rc != PAM_SUCCESS || raw.is_null() {
        return;
    }
    let conv = unsafe { &*(raw as *const PamConv) };
    let Some(callback) = conv.conv else {
        return;
    };

    let message = PamMessage {
        msg_style: style,
        msg: c_text.as_ptr(),
    };
    let mut msg_ptr: *const PamMessage = &message;
    let mut resp: *mut PamResponse = std::ptr::null_mut();

    // SAFETY: one message, pointed to by `msg_ptr`, valid for this call;
    // `resp` is an out-parameter the application allocates.
    let _ = unsafe { callback(1, &mut msg_ptr, &mut resp, conv.appdata_ptr) };

    // The application allocates the response array even for a message that
    // needs no reply; freeing it is the module's job.
    if !resp.is_null() {
        // SAFETY: `resp` is the single-element array the callback allocated.
        unsafe {
            let r = &mut *resp;
            if !r.resp.is_null() {
                free(r.resp as *mut c_void);
            }
            free(resp as *mut c_void);
        }
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
