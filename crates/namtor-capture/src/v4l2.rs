// SPDX-License-Identifier: GPL-3.0-only
//! V4L2 MMAP streaming primitive.
//!
//! A minimal, owned wrapper around the V4L2 memory-mapped streaming path:
//! open → negotiate format → request buffers → mmap → streamon → dequeue →
//! requeue → streamoff. `unsafe` is confined to ioctl invocations and the
//! single `mmap` call, each with a SAFETY comment.
//!
//! Single-planar only — the reference IR/RGB sensors expose
//! `V4L2_BUF_TYPE_VIDEO_CAPTURE`, not `_MPLANE`. Spec §4.11.2: no v4l2
//! ecosystem crate; we talk to the kernel directly through `nix`.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::time::Duration;

use nix::libc::{c_void, timeval};
use thiserror::Error;

/// V4L2 pixel format FourCC, as a `u32` (little-endian on the wire).
pub type PixFmtFourCC = u32;

/// `V4L2_PIX_FMT_GREY` — 8-bit greyscale, the IR sensor's format.
pub const V4L2_PIX_FMT_GREY: PixFmtFourCC = u32::from_le_bytes(*b"GREY");
/// `V4L2_PIX_FMT_YUYV` — YUYV 4:2:2, the RGB sensor's uncompressed format.
pub const V4L2_PIX_FMT_YUYV: PixFmtFourCC = u32::from_le_bytes(*b"YUYV");

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_BUF_TYPE_META_CAPTURE: u32 = 13;
const V4L2_MEMORY_MMAP: u32 = 1;
const V4L2_FIELD_NONE: u32 = 1;

/// Errors from the V4L2 streaming layer.
#[derive(Debug, Error)]
pub enum V4l2Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ioctl failed: {0}")]
    Ioctl(String),
    #[error("device {0} is not a character device")]
    NotCharDevice(String),
    #[error("negotiation refused the requested format")]
    NegotiationRefused,
    #[error("no buffers negotiated")]
    NoBuffers,
    #[error("stream not started")]
    NotStreaming,
}

/// The negotiated image geometry for a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4l2Format {
    pub width: u32,
    pub height: u32,
    pub pixelformat: PixFmtFourCC,
    pub bytesperline: u32,
    pub sizeimage: u32,
}

/// A memory-mapped buffer.
struct MmapBuffer {
    ptr: *mut u8,
    len: usize,
}

// The pointer is to a kernel-owned mapping owned by this `V4l2Stream`. It is
// only dereferenced inside `read_buffer` while the stream holds the mapping.
unsafe impl Send for MmapBuffer {}

/// An owned V4L2 MMAP streaming session. RAII: `Drop` unmaps, streamoffs, and
/// closes the file descriptor.
pub struct V4l2Stream {
    file: File,
    buffers: Vec<Option<MmapBuffer>>,
    format: Option<V4l2Format>,
    started: bool,
    buf_type: u32,
}

impl V4l2Stream {
    pub fn open(path: &Path) -> Result<Self, V4l2Error> {
        Self::open_with_type(path, V4L2_BUF_TYPE_VIDEO_CAPTURE)
    }

    pub fn open_meta(path: &Path) -> Result<Self, V4l2Error> {
        Self::open_with_type(path, V4L2_BUF_TYPE_META_CAPTURE)
    }

    fn open_with_type(path: &Path, buf_type: u32) -> Result<Self, V4l2Error> {
        let meta = std::fs::metadata(path)?;
        if !meta.file_type().is_char_device() {
            return Err(V4l2Error::NotCharDevice(path.display().to_string()));
        }
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(V4l2Error::from)?;
        Ok(Self {
            file,
            buffers: Vec::new(),
            format: None,
            started: false,
            buf_type,
        })
    }

    /// Sets the capture format. The driver may adjust width/height/stride; the
    /// negotiated values are stored and returned.
    pub fn negotiate_format(
        &mut self,
        width: u32,
        height: u32,
        pixelformat: PixFmtFourCC,
    ) -> Result<V4l2Format, V4l2Error> {
        let mut fmt = V4l2FormatStruct::zeroed();
        fmt.type_ = self.buf_type;
        fmt.fmt.pix.width = width;
        fmt.fmt.pix.height = height;
        fmt.fmt.pix.pixelformat = pixelformat;
        fmt.fmt.pix.field = V4L2_FIELD_NONE;

        // SAFETY: VIDIOC_S_FMT is _IOWR('V', 5, struct v4l2_format); `fmt` is
        // a fully-owned stack value of the matching type.
        unsafe { vidioc_s_fmt(self.file.as_raw_fd(), &mut fmt) }
            .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_S_FMT: {e}")))?;

        let negotiated = V4l2Format {
            width: fmt.fmt.pix.width,
            height: fmt.fmt.pix.height,
            pixelformat: fmt.fmt.pix.pixelformat,
            bytesperline: fmt.fmt.pix.bytesperline,
            sizeimage: fmt.fmt.pix.sizeimage,
        };
        self.format = Some(negotiated);
        Ok(negotiated)
    }

    /// Requests `count` MMAP buffers from the driver. Returns the number
    /// actually allocated (which may be fewer).
    pub fn request_buffers(&mut self, count: u32) -> Result<u32, V4l2Error> {
        // A second request on the same stream — Stop then Resume — must first
        // give back the buffers from the first.
        //
        // This used to overwrite `self.buffers` with fresh `None`s, dropping
        // the old `MmapBuffer`s on the floor. `MmapBuffer` has no Drop and the
        // stream's Drop only unmaps what is in the vector at the time, so the
        // first set of mappings was never unmapped. Those mappings keep the
        // driver's vb2 queue busy after the stream itself is gone, so the next
        // open of the node failed `VIDIOC_S_FMT` with EBUSY — for the rest of
        // the process. In a long-lived daemon that meant one Stop -> Resume in
        // enrollment broke every later capture, face unlock included, until a
        // restart. Found by the G1.5 teardown test on the reference camera.
        if !self.buffers.is_empty() {
            let _ = self.streamoff();
            self.unmap_all();
            let mut free = V4l2Requestbuffers {
                count: 0,
                type_: self.buf_type,
                memory: V4L2_MEMORY_MMAP,
                ..Default::default()
            };
            // SAFETY: VIDIOC_REQBUFS with count 0 frees the queue's buffers;
            // every mapping of them was just unmapped above.
            let _ = unsafe { vidioc_reqbufs(self.file.as_raw_fd(), &mut free) };
        }

        let mut rb = V4l2Requestbuffers {
            count,
            type_: self.buf_type,
            memory: V4L2_MEMORY_MMAP,
            ..Default::default()
        };

        // SAFETY: VIDIOC_REQBUFS is _IOWR('V', 8, struct v4l2_requestbuffers).
        unsafe { vidioc_reqbufs(self.file.as_raw_fd(), &mut rb) }
            .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_REQBUFS: {e}")))?;

        self.buffers = (0..rb.count).map(|_| None).collect();
        Ok(rb.count)
    }

    /// Queries and memory-maps every requested buffer.
    pub fn mmap_buffers(&mut self) -> Result<(), V4l2Error> {
        if self.buffers.is_empty() {
            return Err(V4l2Error::NoBuffers);
        }
        // Unmap anything already mapped before mapping again. The assignment
        // below overwrites the slot, and `MmapBuffer` has no `Drop`, so a
        // second call would orphan the first mapping — the same shape as the
        // leak fixed in `request_buffers`, which kept the driver's queue busy
        // and made every later open of the node fail with EBUSY. Every current
        // caller pairs this with `request_buffers`, so this is latent rather
        // than live; the entry point is hardened anyway.
        for mb in self.buffers.iter_mut() {
            if let Some(b) = mb.take() {
                // SAFETY: `b.ptr`/`b.len` describe a mapping obtained below;
                // `take` guarantees it is unmapped at most once.
                unsafe {
                    if let Some(nn) = std::ptr::NonNull::new(b.ptr as *mut c_void) {
                        let _ = nix::sys::mman::munmap(nn, b.len);
                    }
                }
            }
        }
        for idx in 0..self.buffers.len() as u32 {
            let mut buf = V4l2Buffer::zeroed();
            buf.index = idx;
            buf.type_ = self.buf_type;
            buf.memory = V4L2_MEMORY_MMAP;

            // SAFETY: VIDIOC_QUERYBUF is _IOWR('V', 9, struct v4l2_buffer).
            unsafe { vidioc_querybuf(self.file.as_raw_fd(), &mut buf) }
                .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_QUERYBUF[{idx}]: {e}")))?;

            let len = buf.length as usize;
            let offset = buf.m.offset as u64;

            let len_nz = std::num::NonZeroUsize::new(len)
                .ok_or_else(|| V4l2Error::Ioctl(format!("QUERYBUF[{idx}] length=0")))?;

            // SAFETY: `offset` and `len` come straight from QUERYBUF and
            // describe a kernel-owned mapping for this fd. PROT_READ|PROT_WRITE
            // and MAP_SHARED match the streaming contract.
            let ptr = unsafe {
                nix::sys::mman::mmap(
                    None,
                    len_nz,
                    nix::sys::mman::ProtFlags::PROT_READ | nix::sys::mman::ProtFlags::PROT_WRITE,
                    nix::sys::mman::MapFlags::MAP_SHARED,
                    &self.file,
                    offset as nix::libc::off_t,
                )
                .map_err(|e| V4l2Error::Ioctl(format!("mmap[{idx}]: {e}")))?
            };

            self.buffers[idx as usize] = Some(MmapBuffer {
                ptr: ptr.as_ptr() as *mut u8,
                len,
            });
        }
        Ok(())
    }

    /// Starts streaming. All mmap'd buffers must have been queued by the caller
    /// via `requeue` before this is called, per the V4L2 streaming contract.
    pub fn streamon(&mut self) -> Result<(), V4l2Error> {
        if self.started {
            return Ok(());
        }
        let ty: i32 = self.buf_type as i32;
        // SAFETY: VIDIOC_STREAMON is _IOW('V', 18, int); `ty` is the buffer
        // type passed by value through the ioctl.
        unsafe { vidioc_streamon(self.file.as_raw_fd(), &ty) }
            .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_STREAMON: {e}")))?;
        self.started = true;
        Ok(())
    }

    /// Dequeues the next filled buffer, returning its index and the kernel
    /// timestamp. Blocks until a frame is ready.
    pub fn dequeue(&mut self) -> Result<(usize, Duration), V4l2Error> {
        if !self.started {
            return Err(V4l2Error::NotStreaming);
        }
        let mut buf = V4l2Buffer::zeroed();
        buf.type_ = self.buf_type;
        buf.memory = V4L2_MEMORY_MMAP;

        // SAFETY: VIDIOC_DQBUF is _IOWR('V', 17, struct v4l2_buffer).
        unsafe { vidioc_dqbuf(self.file.as_raw_fd(), &mut buf) }
            .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_DQBUF: {e}")))?;

        let ts = buf.timestamp;
        let elapsed = Duration::new(ts.tv_sec as u64, ts.tv_usec as u32 * 1000);
        Ok((buf.index as usize, elapsed))
    }

    /// Requeues a buffer for reuse by the driver.
    pub fn requeue(&mut self, idx: usize) -> Result<(), V4l2Error> {
        let mut buf = V4l2Buffer::zeroed();
        buf.index = idx as u32;
        buf.type_ = self.buf_type;
        buf.memory = V4L2_MEMORY_MMAP;

        // SAFETY: VIDIOC_QBUF is _IOWR('V', 15, struct v4l2_buffer).
        unsafe { vidioc_qbuf(self.file.as_raw_fd(), &mut buf) }
            .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_QBUF[{idx}]: {e}")))?;
        Ok(())
    }

    /// Stops streaming.
    pub fn streamoff(&mut self) -> Result<(), V4l2Error> {
        if !self.started {
            return Ok(());
        }
        let ty: i32 = self.buf_type as i32;
        // SAFETY: VIDIOC_STREAMOFF is _IOW('V', 19, int).
        unsafe { vidioc_streamoff(self.file.as_raw_fd(), &ty) }
            .map_err(|e| V4l2Error::Ioctl(format!("VIDIOC_STREAMOFF: {e}")))?;
        self.started = false;
        Ok(())
    }

    /// Returns the bytes of buffer `idx`. Valid until the buffer is requeued
    /// and refilled by the driver.
    pub fn read_buffer(&self, idx: usize) -> Result<&[u8], V4l2Error> {
        let Some(Some(mb)) = self.buffers.get(idx) else {
            return Err(V4l2Error::NoBuffers);
        };
        // SAFETY: `mb.ptr` points to a mapping this stream owns and keeps
        // mapped for the lifetime of the borrow. `mb.len` is the size from
        // QUERYBUF.
        Ok(unsafe { std::slice::from_raw_parts(mb.ptr, mb.len) })
    }

    /// The negotiated format, if `negotiate_format` has been called.
    pub fn format(&self) -> Option<V4l2Format> {
        self.format
    }
}

impl V4l2Stream {
    /// Unmap every buffer this stream has mapped and forget them. Idempotent.
    fn unmap_all(&mut self) {
        for mb in self.buffers.drain(..).flatten() {
            // SAFETY: `mb.ptr`/`mb.len` describe the mapping obtained via
            // mmap in `mmap_buffers`; munmap with the same length unmaps it,
            // and `drain` guarantees it is never unmapped twice.
            unsafe {
                if let Some(nn) = std::ptr::NonNull::new(mb.ptr as *mut c_void) {
                    if let Err(e) = nix::sys::mman::munmap(nn, mb.len) {
                        // The entry is already drained, so the stream has also
                        // forgotten the mapping exists: say so rather than
                        // lose it silently. This crate carries no logging
                        // dependency, so stderr is the channel available.
                        eprintln!(
                            "namtor: munmap of a {} byte V4L2 buffer failed: {e}",
                            mb.len
                        );
                    }
                }
            }
        }
    }
}

impl Drop for V4l2Stream {
    fn drop(&mut self) {
        let _ = self.streamoff();
        self.unmap_all();
    }
}

// --- kernel ABI structs (single-planar capture only) ---

#[repr(C)]
#[derive(Default)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct V4l2FormatUnion {
    pix: V4l2PixFormat,
    _pad: [u8; 152],
}

#[repr(C, align(8))]
struct V4l2FormatStruct {
    type_: u32,
    _align_pad: [u8; 4],
    fmt: V4l2FormatUnion,
}

impl V4l2FormatStruct {
    fn zeroed() -> Self {
        Self {
            type_: 0,
            _align_pad: [0; 4],
            fmt: V4l2FormatUnion {
                pix: V4l2PixFormat::default(),
                _pad: [0; 152],
            },
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct V4l2Requestbuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C, align(8))]
struct V4l2BufferM {
    offset: u32,
    _pad: [u8; 4],
}

#[repr(C)]
struct V4l2Buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: timeval,
    timecode: V4l2Timecode,
    sequence: u32,
    memory: u32,
    m: V4l2BufferM,
    length: u32,
    reserved2: u32,
    request_fd: i32,
}

impl V4l2Buffer {
    fn zeroed() -> Self {
        Self {
            index: 0,
            type_: 0,
            bytesused: 0,
            flags: 0,
            field: 0,
            timestamp: timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            timecode: V4l2Timecode::default(),
            sequence: 0,
            memory: 0,
            m: V4l2BufferM {
                offset: 0,
                _pad: [0; 4],
            },
            length: 0,
            reserved2: 0,
            request_fd: 0,
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct V4l2Timecode {
    type_: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

// VIDIOC_S_FMT     = _IOWR('V',  5, struct v4l2_format)
nix::ioctl_readwrite!(vidioc_s_fmt, b'V', 5, V4l2FormatStruct);
// VIDIOC_REQBUFS   = _IOWR('V',  8, struct v4l2_requestbuffers)
nix::ioctl_readwrite!(vidioc_reqbufs, b'V', 8, V4l2Requestbuffers);
// VIDIOC_QUERYBUF  = _IOWR('V',  9, struct v4l2_buffer)
nix::ioctl_readwrite!(vidioc_querybuf, b'V', 9, V4l2Buffer);
// VIDIOC_QBUF      = _IOWR('V', 15, struct v4l2_buffer)
nix::ioctl_readwrite!(vidioc_qbuf, b'V', 15, V4l2Buffer);
// VIDIOC_DQBUF     = _IOWR('V', 17, struct v4l2_buffer)
nix::ioctl_readwrite!(vidioc_dqbuf, b'V', 17, V4l2Buffer);
// VIDIOC_STREAMON  = _IOW('V', 18, int)
nix::ioctl_write_ptr!(vidioc_streamon, b'V', 18, std::os::raw::c_int);
// VIDIOC_STREAMOFF = _IOW('V', 19, int)
nix::ioctl_write_ptr!(vidioc_streamoff, b'V', 19, std::os::raw::c_int);

const _: () = {
    assert!(size_of::<V4l2FormatStruct>() == 208);
    assert!(size_of::<V4l2Requestbuffers>() == 20);
    assert!(size_of::<V4l2Buffer>() == 88);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4l2_pix_fmt_grey_is_grey_fourcc() {
        assert_eq!(V4L2_PIX_FMT_GREY, u32::from_le_bytes(*b"GREY"));
        assert_eq!(V4L2_PIX_FMT_GREY, 0x59455247);
    }

    #[test]
    fn v4l2_pix_fmt_yuyv_is_yuyv_fourcc() {
        assert_eq!(V4L2_PIX_FMT_YUYV, u32::from_le_bytes(*b"YUYV"));
    }

    #[test]
    fn v4l2_format_sizeimage_for_640x360_grey_matches_frame_len() {
        // Sanity: 640*360 bytes for 8-bit greyscale.
        let f = V4l2Format {
            width: 640,
            height: 360,
            pixelformat: V4L2_PIX_FMT_GREY,
            bytesperline: 640,
            sizeimage: 640 * 360,
        };
        assert_eq!(f.sizeimage, 230_400);
    }

    #[test]
    fn opening_a_nonexistent_device_is_an_error() {
        assert!(V4l2Stream::open(Path::new("/dev/video-does-not-exist")).is_err());
    }

    #[test]
    #[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
    fn opens_negotiates_streams_and_dequeues_on_reference_hw() {
        if std::env::var("NAMTOR_HW_TEST").is_err() {
            eprintln!("NAMTOR_HW_TEST not set; skipping");
            return;
        }
        let mut s = V4l2Stream::open(Path::new("/dev/video2"))
            .expect("open /dev/video2; check `video` group membership");

        let fmt = s
            .negotiate_format(640, 360, V4L2_PIX_FMT_GREY)
            .expect("negotiate GREY 640x360");
        assert_eq!(fmt.sizeimage, 230_400, "driver should report 640*360 bytes");

        let n = s.request_buffers(4).expect("REQBUFS 4");
        assert!(n >= 2, "need at least 2 buffers, got {n}");
        s.mmap_buffers().expect("mmap");

        for i in 0..n {
            s.requeue(i as usize).expect("QBUF initial");
        }
        s.streamon().expect("STREAMON");

        for _ in 0..2 {
            let (idx, ts) = s.dequeue().expect("DQBUF");
            let buf = s.read_buffer(idx).expect("read_buffer");
            assert_eq!(buf.len(), 230_400, "mmap buffer size");
            assert!(ts.as_secs() < 60, "timestamp sanity");
            s.requeue(idx).expect("requeue");
        }

        s.streamoff().expect("STREAMOFF");
    }
}
