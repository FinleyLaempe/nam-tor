// SPDX-License-Identifier: GPL-3.0-only
//! `namtor worker` subcommand: the sandboxed inference process.
//!
//! Reached as `namtor worker ...` — the daemon re-execs `/proc/self/exe` with
//! this subcommand so the worker is the same *file* but still a separate
//! *process* (spec §3.4: the sandbox is the privilege boundary, not a UID
//! drop). Task 3 wires argument parsing; Task 9 fills in the
//! [`crate::run_main`] loop. Task 6 adds `--seccomp-self-test` for the
//! sandbox escape tests: the process applies seccomp, attempts one action, and
//! reports the errno so the integration test can assert denial.

use std::ffi::CString;
use std::process::ExitCode;

pub const USAGE: &str = "\
Usage: namtor worker --read-fd N --write-fd M [OPTIONS]

Sandboxed inference worker. Re-execed by `namtor daemon`; not meant to be run
by hand.

Options:
      --read-fd N        Request pipe read end
      --write-fd M       Reply pipe write end
      --frame-memfd F    Shared frame memfd
      --frame-len L      Shared frame length in bytes
      --models-dir DIR   Model root override
      --model SPEC       Preload <id>:<quant>:<sha256hex> before sandboxing
                         (repeatable; required under --apply-sandbox because
                         the sandbox denies every filesystem path)
      --apply-sandbox    Apply seccomp + Landlock before serving
  -h, --help             Print this help and exit
  -V, --version          Print the version and exit";

/// Entry point for `namtor worker`. `args` are the arguments *after* the
/// subcommand name.
pub fn main(args: &[String]) -> ExitCode {
    // `handle_known` leaves the worker's own flags alone; only --version and
    // --help are intercepted here.
    if let Some(code) = namtor_layout::cli_preamble::handle_known(
        "namtor worker",
        env!("CARGO_PKG_VERSION"),
        USAGE,
        args,
    ) {
        return ExitCode::from(code);
    }

    // Self-test modes (escape suites) take priority.
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seccomp-self-test" if i + 1 < args.len() => {
                return seccomp_self_test(&args[i + 1]);
            }
            "--landlock-self-test" if i + 1 < args.len() => {
                return landlock_self_test(&args[i + 1]);
            }
            "--apply-sandbox" if i + 2 < args.len() && args[i + 1] == "--self-test" => {
                return combined_escape_test(&args[i + 2]);
            }
            #[cfg(feature = "onnx")]
            "--ioctl-probe" if i + 1 < args.len() => {
                return ioctl_probe(&args[i + 1]);
            }
            _ => {}
        }
        i += 1;
    }

    // Normal worker mode: parse --read-fd, --write-fd, --frame-memfd, --frame-len, --models-dir.
    let mut read_fd: Option<i32> = None;
    let mut write_fd: Option<i32> = None;
    let mut frame_memfd: Option<i32> = None;
    let mut frame_len: u64 = 0;
    let mut apply_sbx = false;
    let mut models_dir: Option<String> = None;
    let mut preload: Vec<namtor_proto::worker::ModelSpec> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--read-fd" if i + 1 < args.len() => {
                read_fd = args[i + 1].parse().ok();
                i += 2;
            }
            "--write-fd" if i + 1 < args.len() => {
                write_fd = args[i + 1].parse().ok();
                i += 2;
            }
            "--frame-memfd" if i + 1 < args.len() => {
                frame_memfd = args[i + 1].parse().ok();
                i += 2;
            }
            "--frame-len" if i + 1 < args.len() => {
                frame_len = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--apply-sandbox" => {
                apply_sbx = true;
                i += 1;
            }
            "--models-dir" if i + 1 < args.len() => {
                models_dir = Some(args[i + 1].clone());
                i += 2;
            }
            // Repeatable. Every model named here is opened and loaded before
            // the sandbox is applied — see `crate::run_main`'s startup order.
            "--model" if i + 1 < args.len() => {
                match crate::model_arg::parse(&args[i + 1]) {
                    Ok(spec) => preload.push(spec),
                    Err(msg) => {
                        eprintln!("namtor worker: {msg}");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other => {
                eprintln!("namtor worker: unknown arg {other}");
                return ExitCode::from(2);
            }
        }
    }

    let (read_fd, write_fd) = match (read_fd, write_fd) {
        (Some(r), Some(w)) => (r, w),
        _ => {
            eprintln!("usage: namtor worker --read-fd N --write-fd M [--frame-memfd F --frame-len L] [--model SPEC]... [--apply-sandbox]");
            return ExitCode::from(2);
        }
    };

    let backend = crate::infer::default_backend();
    let backend = with_models_dir(backend, models_dir.as_deref());
    let code = match crate::run_main(
        read_fd,
        write_fd,
        backend,
        apply_sbx,
        frame_memfd,
        frame_len,
        &preload,
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("namtor worker: {e}");
            1
        }
    };
    exit_without_teardown(code)
}

/// Leave the process immediately, skipping libc `atexit` handlers and the Rust
/// runtime's own cleanup.
///
/// Measured, not guessed: with the seccomp default action temporarily set to
/// `SCMP_ACT_TRAP` the worker completes `Load`, `Infer` and the sealed-`Load`
/// refusal without a single trap, and then dies with `SIGSYS` on syscall 131
/// (`sigaltstack`) from `std::rt::cleanup` tearing down the stack-overflow
/// guard handler. Under the shipped `SCMP_ACT_ERRNO(EPERM)` action that
/// `sigaltstack` merely fails, and teardown runs on into ONNX Runtime's
/// `CPUIDInfo` destructor → `cpuinfo_deinitialize` → `pthread_once`, whose
/// glibc futex helper treats the `EPERM` from the denied `futex` as
/// unrecoverable and calls `__libc_fatal`, aborting the process.
///
/// Both denials are in *teardown only* — the request loop itself needs nothing
/// outside the allowlist. So the fix is to not run teardown rather than to
/// admit `sigaltstack` and `futex` into the filter: widening it would buy a
/// tidier exit status at the cost of the syscall surface, and `futex` in
/// particular is a building block for the thread machinery the filter exists
/// to keep out.
///
/// Nothing is lost by skipping it. Every reply is flushed by
/// [`crate::pipe::PipeTransport::send`] as it is written, the worker holds no
/// on-disk state, and the kernel closes the fds and unmaps the frame region on
/// exit regardless. `stdout`/`stderr` are flushed here because those *are*
/// buffered by the Rust runtime.
///
/// This runs on the sandboxed and unsandboxed paths alike, so the exit path
/// that ships is the one the tests exercise.
fn exit_without_teardown(code: i32) -> ExitCode {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` is async-signal-safe and does not return. It issues
    // `exit_group`, which the seccomp allowlist permits.
    unsafe { libc::_exit(code) }
}

/// Point the real ONNX backend at `--models-dir` when one was given.
///
/// Without the `onnx` feature there is no `OnnxCpu` to re-root, so the
/// deterministic mock backend is returned unchanged.
#[cfg(feature = "onnx")]
fn with_models_dir(
    backend: Box<dyn crate::infer::InferenceBackend>,
    models_dir: Option<&str>,
) -> Box<dyn crate::infer::InferenceBackend> {
    match models_dir {
        Some(dir) => {
            Box::new(crate::infer_onnx::OnnxCpu::new(1).with_root(std::path::PathBuf::from(dir)))
        }
        None => backend,
    }
}

#[cfg(not(feature = "onnx"))]
fn with_models_dir(
    backend: Box<dyn crate::infer::InferenceBackend>,
    _models_dir: Option<&str>,
) -> Box<dyn crate::infer::InferenceBackend> {
    backend
}

/// Create a `CString` from a `&'static str` literal known to be NUL-free.
///
/// `CString::new` only fails on interior NUL bytes; the string literals passed
/// here are compile-time constants without NULs, so this is infallible. Used in
/// self-test functions that return `TestResult` (not `Result`) and therefore
/// cannot propagate the error via `?`.
fn cstr(s: &'static str) -> CString {
    CString::new(s).unwrap_or_else(|e| {
        eprintln!("internal: NUL in static literal {s:?}: {e}");
        std::process::exit(2);
    })
}

/// Apply seccomp then attempt `kind`; print the errno (or "ok") and exit.
/// Exit code 0 = action succeeded, 1 = action denied (EPERM), 2 = usage.
fn seccomp_self_test(kind: &str) -> ExitCode {
    if let Err(e) = crate::sandbox::seccomp::apply_seccomp() {
        eprintln!("seccomp apply failed: {e}");
        return ExitCode::FAILURE;
    }
    let result = match kind {
        "open" => test_open(),
        "socket" => test_socket(),
        "write" => test_write(),
        "mprotect_exec" => test_mprotect_exec(),
        "mmap_exec" => test_mmap_exec(),
        "mmap_noexec" => test_mmap_noexec(),
        other => {
            eprintln!("unknown self-test kind: {other}");
            return ExitCode::from(2);
        }
    };
    match result {
        TestResult::Ok => {
            println!("ok");
            ExitCode::SUCCESS
        }
        TestResult::Errno(n) => {
            println!("errno {n}");
            ExitCode::from(1)
        }
    }
}

enum TestResult {
    Ok,
    Errno(i32),
}

fn test_open() -> TestResult {
    test_open_path("/etc/passwd")
}

/// Attempt to `open(2)` an arbitrary filesystem path. Under the sandbox,
/// seccomp denies `open`/`openat` with EPERM before the kernel checks
/// whether the path exists — so this returns EPERM even for paths that
/// do not exist on the test host. This is the proof that the worker
/// cannot read templates, keys, or model files after the sandbox is
/// applied, regardless of its UID.
fn test_open_path(path: &str) -> TestResult {
    use std::os::raw::c_int;
    extern "C" {
        fn open(path: *const std::os::raw::c_char, flags: c_int) -> c_int;
    }
    let cpath = match CString::new(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("test_open_path: NUL in path: {e}");
            return TestResult::Errno(0);
        }
    };
    // SAFETY: open is a libc syscall; cpath is a valid NUL-terminated C string.
    // O_RDONLY = 0 on Linux.
    let fd = unsafe { open(cpath.as_ptr(), 0) };
    if fd < 0 {
        TestResult::Errno(errno())
    } else {
        extern "C" {
            fn close(fd: c_int) -> c_int;
        }
        unsafe { close(fd) };
        TestResult::Ok
    }
}

fn test_socket() -> TestResult {
    use std::os::raw::c_int;
    extern "C" {
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
    }
    // AF_UNIX = 1, SOCK_STREAM = 1, 0 protocol.
    let fd = unsafe { socket(1, 1, 0) };
    if fd < 0 {
        TestResult::Errno(errno())
    } else {
        extern "C" {
            fn close(fd: c_int) -> c_int;
        }
        unsafe { close(fd) };
        TestResult::Ok
    }
}

fn test_write() -> TestResult {
    use std::io::Write;
    // Writing to stdout (fd 1) must succeed under the filter.
    let _ = std::io::stdout().write_all(b"");
    if std::io::stdout().flush().is_ok() {
        TestResult::Ok
    } else {
        TestResult::Errno(errno())
    }
}

// ---- SEC-6 W^X self-tests ----

use std::os::raw::{c_int, c_void};

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const PROT_EXEC: c_int = 4;
const MAP_PRIVATE: c_int = 0x02;
const MAP_ANONYMOUS: c_int = 0x20;
const PAGE: usize = 4096;

extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

/// SEC-6 W^X: `mmap(NULL, PAGE, PROT_EXEC, MAP_PRIVATE|MAP_ANON, -1, 0)` must
/// be denied by the conditional allow rule (the `PROT_EXEC` bit fails the
/// `MaskedEq(PROT_EXEC) == 0` check, so the default EPERM action applies).
fn test_mmap_exec() -> TestResult {
    // SAFETY: mmap with PROT_EXEC on a fresh anonymous page; under the W^X
    // seccomp filter this returns MAP_FAILED with errno EPERM.
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            PAGE,
            PROT_EXEC,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        TestResult::Errno(errno())
    } else {
        unsafe { munmap(ptr, PAGE) };
        TestResult::Ok
    }
}

/// SEC-6 W^X positive control: `mmap(NULL, PAGE, PROT_READ|PROT_WRITE,
/// MAP_PRIVATE|MAP_ANON, -1, 0)` must succeed — the conditional allow rule
/// passes because `(prot & PROT_EXEC) == 0`.
fn test_mmap_noexec() -> TestResult {
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            PAGE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return TestResult::Errno(errno());
    }
    unsafe { munmap(ptr, PAGE) };
    TestResult::Ok
}

/// SEC-6 W^X: first mmap a page without `PROT_EXEC` (allowed), then attempt
/// `mprotect(page, PAGE, PROT_EXEC)` — must be denied. This proves the
/// `mprotect` gate closes the bypass that would exist if only `mmap` were
/// filtered.
fn test_mprotect_exec() -> TestResult {
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            PAGE,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        eprintln!("mprotect_exec: setup mmap failed: errno {}", errno());
        return TestResult::Errno(errno());
    }
    // SAFETY: mprotect on a valid page; under the W^X filter this returns
    // -1 with errno EPERM.
    let ret = unsafe { mprotect(ptr, PAGE, PROT_EXEC) };
    unsafe { munmap(ptr, PAGE) };
    if ret < 0 {
        TestResult::Errno(errno())
    } else {
        TestResult::Ok
    }
}

/// Read the thread-local `errno` via `__errno_location`.
fn errno() -> i32 {
    extern "C" {
        fn __errno_location() -> *mut std::os::raw::c_int;
    }
    // SAFETY: __errno_location returns a pointer to a thread-local int.
    unsafe { *__errno_location() }
}

/// Apply landlock then attempt `kind`; print the errno (or "ok") and exit.
fn landlock_self_test(kind: &str) -> ExitCode {
    if let Err(e) = crate::sandbox::landlock::apply_landlock() {
        eprintln!("landlock apply failed: {e}");
        return ExitCode::FAILURE;
    }
    let result = match kind {
        "open" => test_open(),
        "mkdir" => test_mkdir(),
        "read_inherited" => test_read_inherited(),
        other => {
            eprintln!("unknown landlock self-test kind: {other}");
            return ExitCode::from(2);
        }
    };
    match result {
        TestResult::Ok => {
            println!("ok");
            ExitCode::SUCCESS
        }
        TestResult::Errno(n) => {
            println!("errno {n}");
            ExitCode::from(1)
        }
    }
}

fn test_mkdir() -> TestResult {
    extern "C" {
        fn mkdir(path: *const std::os::raw::c_char, mode: u32) -> std::os::raw::c_int;
    }
    let path = cstr("/tmp/namtor-landlock-test");
    // SAFETY: mkdir is a libc syscall; path is a valid NUL-terminated C string.
    let ret = unsafe { mkdir(path.as_ptr(), 0o755) };
    if ret < 0 {
        TestResult::Errno(errno())
    } else {
        extern "C" {
            fn rmdir(path: *const std::os::raw::c_char) -> std::os::raw::c_int;
        }
        unsafe { rmdir(path.as_ptr()) };
        TestResult::Ok
    }
}

fn test_read_inherited() -> TestResult {
    // Read from stdin (fd 0), which is inherited and pre-opened. Even a short
    // read or EOF counts as "ok" — the point is that read() on an inherited
    // fd is not denied by landlock (landlock restricts paths, not fds).
    use std::io::Read;
    let mut buf = [0u8; 1];
    match std::io::stdin().read(&mut buf) {
        Ok(0) | Ok(_) => TestResult::Ok,
        Err(e) => TestResult::Errno(e.raw_os_error().unwrap_or(0)),
    }
}

/// Combined sandbox escape test: apply both landlock + seccomp, then attempt
/// the named escape kind. Used by `tests/escape.rs` (G5.2).
fn combined_escape_test(kind: &str) -> ExitCode {
    if let Err(e) = crate::sandbox::apply_sandbox() {
        eprintln!("sandbox apply failed: {e}");
        return ExitCode::FAILURE;
    }
    let result = match kind {
        "open_file" => test_open(),
        "open_templates" => test_open_path("/var/lib/namtor/templates/0.bin"),
        "open_keys" => test_open_path("/var/lib/namtor/templates.key/key.bin"),
        "open_models" => test_open_path("/var/lib/namtor/models/yunet_2023mar/model.onnx"),
        "open_socket" => test_socket(),
        "connect" => test_connect(),
        "exec" => test_exec(),
        "fork" => test_fork(),
        "write_inherited" => test_write(),
        "mprotect_exec" => test_mprotect_exec(),
        "mmap_exec" => test_mmap_exec(),
        "mmap_noexec" => test_mmap_noexec(),
        other => {
            eprintln!("unknown combined self-test kind: {other}");
            return ExitCode::from(2);
        }
    };
    match result {
        TestResult::Ok => {
            println!("ok");
            ExitCode::SUCCESS
        }
        TestResult::Errno(n) => {
            println!("errno {n}");
            ExitCode::from(1)
        }
    }
}

fn test_connect() -> TestResult {
    use std::os::raw::c_int;
    extern "C" {
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;
        fn connect(fd: c_int, addr: *const std::os::raw::c_void, len: u32) -> c_int;
    }
    // AF_UNIX = 1, SOCK_STREAM = 1
    let fd = unsafe { socket(1, 1, 0) };
    if fd < 0 {
        return TestResult::Errno(errno());
    }
    // Minimal sockaddr_un for abstract namespace "\0test".
    #[repr(C)]
    struct SockAddrUnix {
        family: u16,
        path: [u8; 108],
    }
    let mut addr = SockAddrUnix {
        family: 1,
        path: [0u8; 108],
    };
    addr.path[1] = b't';
    let ret = unsafe { connect(fd, &addr as *const _ as *const _, 110) };
    extern "C" {
        fn close(fd: c_int) -> c_int;
    }
    unsafe { close(fd) };
    if ret < 0 {
        TestResult::Errno(errno())
    } else {
        TestResult::Ok
    }
}

fn test_exec() -> TestResult {
    extern "C" {
        fn execve(
            path: *const std::os::raw::c_char,
            argv: *const *const std::os::raw::c_char,
            envp: *const *const std::os::raw::c_char,
        ) -> std::os::raw::c_int;
    }
    let path = cstr("/bin/true");
    let null_ptr: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: execve only returns on error. If it succeeds, the process is
    // replaced — but the sandbox should deny it before that happens.
    let ret = unsafe { execve(path.as_ptr(), &null_ptr, &null_ptr) };
    if ret < 0 {
        TestResult::Errno(errno())
    } else {
        TestResult::Ok
    }
}

fn test_fork() -> TestResult {
    extern "C" {
        fn fork() -> std::os::raw::c_int;
    }
    // SAFETY: fork is denied by seccomp; if it somehow succeeds, the child
    // exits immediately.
    let ret = unsafe { fork() };
    if ret < 0 {
        TestResult::Errno(errno())
    } else if ret == 0 {
        std::process::exit(0);
    } else {
        extern "C" {
            fn waitpid(
                pid: std::os::raw::c_int,
                status: *mut std::os::raw::c_int,
                opts: std::os::raw::c_int,
            ) -> std::os::raw::c_int;
        }
        let mut status: std::os::raw::c_int = 0;
        unsafe { waitpid(ret, &mut status, 0) };
        TestResult::Ok
    }
}

/// Diagnostic probe: load `model_path` with OnnxCpu, run inference repeatedly,
/// and print markers so `strace -e ioctl` between PROBE_START/PROBE_END captures
/// the ioctl codes the ONNX Runtime issues during inference (after load). Used
/// by SEC-6 task 9 to measure the ioctl surface before narrowing the seccomp
/// allowlist. No sandbox is applied — the probe exists to observe the
/// unrestricted syscall surface.
#[cfg(feature = "onnx")]
fn ioctl_probe(model_path: &str) -> ExitCode {
    let path = std::path::Path::new(model_path);
    let mut backend = crate::infer_onnx::OnnxCpu::new(1);
    let spec = namtor_proto::worker::ModelSpec {
        id: "probe".into(),
        quant: namtor_proto::worker::Quant::Fp32,
        sha256: [0; 32],
    };
    let loaded = match backend.load_file(path, &spec) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("probe load failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Use a zero frame whose size matches the loaded input shape product.
    let n: usize = loaded
        .input_shape
        .iter()
        .map(|d| (*d as usize).max(1))
        .product::<usize>()
        .max(1);
    let frame: Vec<u8> = vec![0u8; n * 4];
    let frame_ref = namtor_proto::worker::FrameRef {
        offset: 0,
        len: (n * 4) as u32,
        emitter: namtor_capture::frame::EmitterState::On,
        spectrum: namtor_capture::source::Spectrum::Ir,
        pix_fmt: namtor_capture::frame::PixFmt::Grey8,
        w: n as u32,
        h: 1,
        ts_us: 0,
    };
    println!("PROBE_START");
    use crate::infer::InferenceBackend;
    for _ in 0..5 {
        if let Err(e) = backend.run(&loaded, &frame, &frame_ref) {
            eprintln!("probe run failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("PROBE_END");
    ExitCode::SUCCESS
}
