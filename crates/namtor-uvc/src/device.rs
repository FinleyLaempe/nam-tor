// SPDX-License-Identifier: Apache-2.0
//! Live V4L2 device transport for extension unit queries.

use crate::probe::{XuError, XuTransport};
use crate::xu::{uvc_ctrl_query, Query, UvcXuControlQuery};
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

/// A V4L2 device node, used only as an ioctl target for XU queries.
pub struct V4l2Device {
    file: File,
}

impl V4l2Device {
    pub fn open(path: &Path) -> Result<Self, XuError> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| XuError::Ioctl(format!("open {}: {e}", path.display())))?;
        Ok(Self { file })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl XuTransport for V4l2Device {
    fn query(&self, unit: u8, selector: u8, query: Query, buf: &mut [u8]) -> Result<(), XuError> {
        let mut q = UvcXuControlQuery {
            unit,
            selector,
            query: query as u8,
            size: buf.len() as u16,
            data: buf.as_mut_ptr().cast(),
        };

        // SAFETY: `q.data` points at `buf`, which outlives this call, and
        // `q.size` is exactly `buf.len()`. The kernel reads or writes at most
        // `size` bytes there.
        let rc = unsafe { uvc_ctrl_query(self.fd(), &mut q) };

        rc.map(|_| ())
            .map_err(|e| XuError::Ioctl(format!("UVCIOC_CTRL_QUERY {query:?}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::SafeXu;
    use std::path::Path;

    fn hw_enabled() -> bool {
        std::env::var("NAMTOR_HW_TEST").is_ok()
    }

    #[test]
    fn opening_a_nonexistent_device_is_an_error() {
        assert!(V4l2Device::open(Path::new("/dev/video-does-not-exist")).is_err());
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn reference_camera_ir_node_opens() {
        if !hw_enabled() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let dev = V4l2Device::open(Path::new("/dev/video2"))
            .expect("IR node /dev/video2 must be openable; check `video` group membership");
        assert!(dev.fd() >= 0);
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn xu_query_returns_either_data_or_a_clean_error_never_a_hang() {
        // Answers a real unknown: whether unmapped XU access needs elevated
        // privilege on this kernel. EPERM here is a finding, not a failure.
        if !hw_enabled() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let dev = V4l2Device::open(Path::new("/dev/video2")).unwrap();
        let mut xu = SafeXu::new(dev);

        // Unit 4 is the MSXU on the IR VideoControl interface, and it
        // advertises selectors 6 and 9 (bmControls 20 01). These are the
        // primary emitter candidates on this device.
        for selector in [6u8, 9u8] {
            match xu.probe_control(4, selector) {
                Ok(p) => eprintln!(
                    "unit 4 selector {selector}: len={} info={:?} current={:02x?}",
                    p.len, p.info, p.current
                ),
                Err(e) => eprintln!("unit 4 selector {selector}: {e}"),
            }
        }
    }
}
