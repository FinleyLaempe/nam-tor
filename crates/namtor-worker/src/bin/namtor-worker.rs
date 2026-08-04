// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-worker` binary: the sandboxed inference process.
//!
//! Task 3 wires argument parsing; Task 9 fills in the
//! [`namtor_worker::run_main`] loop. Task 6 adds `--seccomp-self-test` for the
//! sandbox escape tests: the binary applies seccomp, attempts one action, and
//! reports the errno so the integration test can assert denial.

use std::ffi::CString;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
            "--read-fd" if i + 1 < args.len() => {
                i += 2;
            }
            "--write-fd" if i + 1 < args.len() => {
                i += 2;
            }
            other => {
                eprintln!("namtor-worker: unknown arg {other}");
                return ExitCode::from(2);
            }
        }
    }
    eprintln!("usage: namtor-worker --read-fd N --write-fd M");
    ExitCode::from(2)
}

/// Apply seccomp then attempt `kind`; print the errno (or "ok") and exit.
/// Exit code 0 = action succeeded, 1 = action denied (EPERM), 2 = usage.
fn seccomp_self_test(kind: &str) -> ExitCode {
    if let Err(e) = namtor_worker::sandbox::seccomp::apply_seccomp() {
        eprintln!("seccomp apply failed: {e}");
        return ExitCode::FAILURE;
    }
    let result = match kind {
        "open" => test_open(),
        "socket" => test_socket(),
        "write" => test_write(),
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
    use std::ffi::CString;
    use std::os::raw::c_int;
    extern "C" {
        fn open(path: *const std::os::raw::c_char, flags: c_int) -> c_int;
    }
    let path = CString::new("/etc/passwd").unwrap();
    // SAFETY: open is a libc syscall; path is a valid NUL-terminated C string.
    // O_RDONLY = 0 on Linux.
    let fd = unsafe { open(path.as_ptr(), 0) };
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
    if let Err(e) = namtor_worker::sandbox::landlock::apply_landlock() {
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
    let path = CString::new("/tmp/namtor-landlock-test").unwrap();
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
    if let Err(e) = namtor_worker::sandbox::apply_sandbox() {
        eprintln!("sandbox apply failed: {e}");
        return ExitCode::FAILURE;
    }
    let result = match kind {
        "open_file" => test_open(),
        "open_socket" => test_socket(),
        "connect" => test_connect(),
        "exec" => test_exec(),
        "fork" => test_fork(),
        "write_inherited" => test_write(),
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
    use std::ffi::CString;
    extern "C" {
        fn execve(
            path: *const std::os::raw::c_char,
            argv: *const *const std::os::raw::c_char,
            envp: *const *const std::os::raw::c_char,
        ) -> std::os::raw::c_int;
    }
    let path = CString::new("/bin/true").unwrap();
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
