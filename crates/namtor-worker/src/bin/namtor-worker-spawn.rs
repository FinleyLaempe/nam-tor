// SPDX-License-Identifier: GPL-3.0-only
//! `namtor-worker-spawn`: setuid root helper that drops to `nobody` and execs
//! `namtor-worker`.
//!
//! This is the only setuid binary in the system. The daemon (user `namtor`)
//! invokes it to start the worker; the helper drops privileges to `nobody`
//! and execs the worker, which self-applies the sandbox (seccomp + landlock).
//!
//! In CI (no root), `--dry-run` parses args and prints the planned
//! setuid/setgid/exec without running.

use std::ffi::CString;
use std::process::ExitCode;

#[derive(Debug)]
struct SpawnArgs {
    worker: String,
    read_fd: i32,
    write_fd: i32,
    frame_memfd: i32,
    frame_len: u64,
    extra: Vec<String>,
    dry_run: bool,
    self_test: bool,
}

fn print_usage() {
    eprintln!(
        "usage: namtor-worker-spawn --worker <path> --read-fd N --write-fd M \
         --frame-memfd F --frame-len L [--worker-arg <arg>]... [--dry-run] [--self-test]"
    );
}

fn parse_args(args: &[String]) -> Result<SpawnArgs, String> {
    let mut worker = String::new();
    let mut read_fd: Option<i32> = None;
    let mut write_fd: Option<i32> = None;
    let mut frame_memfd: Option<i32> = None;
    let mut frame_len: u64 = 0;
    let mut extra: Vec<String> = Vec::new();
    let mut dry_run = false;
    let mut self_test = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--worker" if i + 1 < args.len() => {
                worker = args[i + 1].clone();
                i += 2;
            }
            "--read-fd" if i + 1 < args.len() => {
                read_fd = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| format!("--read-fd: {e}"))?,
                );
                i += 2;
            }
            "--write-fd" if i + 1 < args.len() => {
                write_fd = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| format!("--write-fd: {e}"))?,
                );
                i += 2;
            }
            "--frame-memfd" if i + 1 < args.len() => {
                frame_memfd = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|e: std::num::ParseIntError| format!("--frame-memfd: {e}"))?,
                );
                i += 2;
            }
            "--frame-len" if i + 1 < args.len() => {
                frame_len = args[i + 1]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| format!("--frame-len: {e}"))?;
                i += 2;
            }
            "--worker-arg" if i + 1 < args.len() => {
                extra.push(args[i + 1].clone());
                i += 2;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--self-test" => {
                self_test = true;
                i += 1;
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }

    if worker.is_empty() {
        return Err("--worker is required".into());
    }
    let read_fd = read_fd.ok_or("--read-fd is required")?;
    let write_fd = write_fd.ok_or("--write-fd is required")?;
    let frame_memfd = frame_memfd.ok_or("--frame-memfd is required")?;

    Ok(SpawnArgs {
        worker,
        read_fd,
        write_fd,
        frame_memfd,
        frame_len,
        extra,
        dry_run,
        self_test,
    })
}

fn drop_and_exec(args: &SpawnArgs) -> Result<(), String> {
    let nobody_uid = nix::unistd::User::from_name("nobody")
        .map_err(|e| format!("getpwnam(nobody): {e}"))?
        .ok_or_else(|| "user 'nobody' not found".to_string())?
        .uid;
    let nobody_gid = nix::unistd::Group::from_name("nogroup")
        .map_err(|e| format!("getgrnam(nogroup): {e}"))?
        .ok_or_else(|| "group 'nogroup' not found".to_string())?
        .gid;

    nix::unistd::setgid(nobody_gid).map_err(|e| format!("setgid({nobody_gid}): {e}"))?;
    nix::unistd::setuid(nobody_uid).map_err(|e| format!("setuid({nobody_uid}): {e}"))?;

    let worker_c = CString::new(args.worker.as_str()).map_err(|e| format!("worker path: {e}"))?;
    let read_fd_c = CString::new(args.read_fd.to_string()).map_err(|e| format!("read_fd: {e}"))?;
    let write_fd_c = CString::new(args.write_fd.to_string()).map_err(|e| format!("write_fd: {e}"))?;
    let frame_memfd_c = CString::new(args.frame_memfd.to_string()).map_err(|e| format!("frame_memfd: {e}"))?;
    let frame_len_c = CString::new(args.frame_len.to_string()).map_err(|e| format!("frame_len: {e}"))?;
    let apply_c = CString::new("--apply-sandbox").map_err(|e| format!("--apply-sandbox: {e}"))?;
    let read_fd_flag = CString::new("--read-fd").map_err(|e| format!("--read-fd: {e}"))?;
    let write_fd_flag = CString::new("--write-fd").map_err(|e| format!("--write-fd: {e}"))?;
    let frame_memfd_flag = CString::new("--frame-memfd").map_err(|e| format!("--frame-memfd: {e}"))?;
    let frame_len_flag = CString::new("--frame-len").map_err(|e| format!("--frame-len: {e}"))?;

    let mut argv_c: Vec<CString> = vec![
        worker_c.clone(),
        read_fd_flag,
        read_fd_c,
        write_fd_flag,
        write_fd_c,
        frame_memfd_flag,
        frame_memfd_c,
        frame_len_flag,
        frame_len_c,
        apply_c,
    ];
    for a in &args.extra {
        argv_c.push(CString::new(a.as_str()).map_err(|e| format!("worker-arg: {e}"))?);
    }

    nix::unistd::execvp(&worker_c, &argv_c).map_err(|e| format!("execvp({}): {e}", args.worker))?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("namtor-worker-spawn: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    if parsed.self_test {
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        println!("uid={uid} gid={gid}");
        return ExitCode::SUCCESS;
    }

    if parsed.dry_run {
        println!(
            "dry-run: worker={} read_fd={} write_fd={} frame_memfd={} frame_len={} extra={:?}",
            parsed.worker,
            parsed.read_fd,
            parsed.write_fd,
            parsed.frame_memfd,
            parsed.frame_len,
            parsed.extra
        );
        return ExitCode::SUCCESS;
    }

    match drop_and_exec(&parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("namtor-worker-spawn: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> String {
        s.into()
    }

    #[test]
    fn dry_run_parses_all_args() {
        let args = vec![
            a("--worker"),
            a("/usr/lib/namtor/namtor-worker"),
            a("--read-fd"),
            a("3"),
            a("--write-fd"),
            a("4"),
            a("--frame-memfd"),
            a("5"),
            a("--frame-len"),
            a("67108864"),
            a("--worker-arg"),
            a("--foo"),
            a("--dry-run"),
        ];
        let p = parse_args(&args).expect("parse");
        assert_eq!(p.worker, "/usr/lib/namtor/namtor-worker");
        assert_eq!(p.read_fd, 3);
        assert_eq!(p.write_fd, 4);
        assert_eq!(p.frame_memfd, 5);
        assert_eq!(p.frame_len, 67_108_864);
        assert_eq!(p.extra, vec!["--foo".to_string()]);
        assert!(p.dry_run);
    }

    #[test]
    fn missing_worker_errors() {
        let args = vec![a("--read-fd"), a("3"), a("--write-fd"), a("4")];
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("--worker"), "got: {err}");
    }

    #[test]
    fn missing_fd_errors() {
        let args = vec![a("--worker"), a("/x")];
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("--read-fd"), "got: {err}");
    }

    #[test]
    fn unknown_arg_errors() {
        let args = vec![a("--bogus")];
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("unknown"), "got: {err}");
    }

    #[test]
    fn non_integer_fd_errors() {
        let args = vec![a("--worker"), a("/x"), a("--read-fd"), a("abc")];
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("--read-fd"), "got: {err}");
    }
}
