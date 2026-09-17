// SPDX-License-Identifier: GPL-3.0-only
//! Gate **G1.5** — "emitter off ≤ 200 ms after every teardown path" — for the
//! four paths the gate names: end, stop, SIGKILL, and suspend.
//!
//! # What this measures, and what it does not
//!
//! The earlier measurement (`enrollment_live.rs`,
//! `emitter_goes_dark_within_the_teardown_budget`) timed teardown in
//! **photons**, using the camera's own frames. That witness goes blind: the
//! MSXU mode switch stops the video stream for ~390 ms in either direction,
//! so it could only bracket the moment the LED went dark to [42.7, 435.0] ms.
//! It also drove the library directly, one path, never the daemon's own code.
//!
//! This file uses a witness that does not go blind: `GET_CUR` on the UVC
//! **control** endpoint, which is a separate USB transfer from the video
//! stream and keeps answering through the mode switch. It reports when the
//! **device** holds `payload_off` — the control plane. It does not report
//! photons; a device could in principle accept the control write before the
//! LED current actually falls, and only an independent optical observer (a
//! photodiode, or a second camera watching the LED) closes that last gap.
//! Read the numbers below as "the device has been commanded off and says so",
//! not "no infrared is leaving the module".
//!
//! Each path is driven through the code the shipped system actually runs:
//!
//! | path    | code under test                                                    |
//! |---------|--------------------------------------------------------------------|
//! | end     | the daemon's capture factory, then drop (`EmitterGuard::drop`)     |
//! | stop    | the daemon's capture factory, then `CaptureSource::stop`           |
//! | suspend | a real `Daemon` whose `release()` is what `release_for_suspend` calls |
//! | SIGKILL | a separate process killed with SIGKILL, then `namtor emitter-off`  |
//!
//! `#[ignore]`d: requires the reference camera. Run with
//! `NAMTOR_HW_TEST=1 cargo test -p namtor-daemon --test emitter_teardown_paths -- --ignored --nocapture --test-threads=1`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use namtor_camera::inventory::SysfsInventory;
use namtor_daemon::daemon::Daemon;
use namtor_daemon::lifecycle::SystemClock;
use namtor_daemon::live_capture::factory_with_inventory;
use namtor_daemon::supervisor::MockSupervisor;
use namtor_hwdb::quirks::EmitterCfg;
use namtor_proto::pam::Requester;
use namtor_proto::worker::{ModelSpec, Quant};

#[allow(dead_code)]
mod hw_emitter;

const IR_NODE: &str = "/dev/video2";
/// The gate's budget.
const TEARDOWN_BUDGET_MS: f64 = 200.0;
/// How long to keep polling before calling a path a failure outright.
const GIVE_UP_MS: u64 = 1500;
/// Samples per path. Enough to show the spread, few enough that five paths of
/// arm/disarm cycles do not overheat anything.
const SAMPLES: usize = 5;

/// Serialises the tests in this binary: they share one physical camera.
static CAMERA: Mutex<()> = Mutex::new(());

fn hw_enabled() -> bool {
    std::env::var("NAMTOR_HW_TEST").as_deref() == Ok("1")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn quirks_dir() -> PathBuf {
    repo_root().join("quirks/devices")
}

fn reference_cfg() -> Option<EmitterCfg> {
    hw_emitter::emitter_cfg(&[quirks_dir()], hw_emitter::REFERENCE_VID_PID).map(|(_, c)| c)
}

/// `current` holds `payload`. GET_CUR can return the control's full length
/// while the quirks payload is written at the same length, so compare the
/// payload's own span.
fn holds(current: &[u8], payload: &[u8]) -> bool {
    current.len() >= payload.len() && current[..payload.len()] == *payload
}

/// Poll the control endpoint from `t0` until the device reports `payload_off`.
/// Returns the elapsed milliseconds, or an error naming what it last read.
fn time_until_off(t0: Instant, cfg: &EmitterCfg) -> Result<f64, String> {
    let node = Path::new(IR_NODE);
    let mut last = Vec::new();
    loop {
        // A read error is not fatal: the node can be briefly busy while
        // another handle closes, so keep polling rather than fail on it.
        if let Ok(cur) = hw_emitter::read_control(node, cfg) {
            if holds(&cur, &cfg.payload_off) {
                return Ok(t0.elapsed().as_secs_f64() * 1000.0);
            }
            last = cur;
        }
        if t0.elapsed() > Duration::from_millis(GIVE_UP_MS) {
            return Err(format!(
                "still not payload_off after {GIVE_UP_MS} ms; last GET_CUR {last:02x?}, \
                 expected {:02x?}",
                cfg.payload_off
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn assert_armed(cfg: &EmitterCfg, when: &str) {
    let cur = hw_emitter::read_control(Path::new(IR_NODE), cfg)
        .unwrap_or_else(|e| panic!("GET_CUR {when}: {e}"));
    assert!(
        holds(&cur, &cfg.payload_on),
        "{when}: expected the emitter armed (payload_on {:02x?}), GET_CUR read {cur:02x?}. \
         Nothing to time if it was never on.",
        cfg.payload_on
    );
}

/// The daemon's own capture factory, over real sysfs and the repo quirks.
fn daemon_factory() -> namtor_daemon::daemon::CaptureFactory {
    let inv = SysfsInventory::new(PathBuf::from("/sys"), vec![quirks_dir()]);
    factory_with_inventory(Box::new(inv), None, None)
}

fn summarise(path: &str, samples: &[f64]) {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = s[s.len() / 2];
    let max = *s.last().unwrap();
    eprintln!(
        "G1.5 control-plane teardown [{path}]: samples {:?} ms — median {median:.1}, max {max:.1}, \
         budget {TEARDOWN_BUDGET_MS:.0}",
        s.iter().map(|v| (v * 10.0).round() / 10.0).collect::<Vec<_>>()
    );
    assert!(
        max <= TEARDOWN_BUDGET_MS,
        "[{path}] max {max:.1} ms exceeds the {TEARDOWN_BUDGET_MS:.0} ms budget"
    );
}

/// Point the daemon's emitter-state record at a scratch file, so the factory
/// does not try to write under /var/lib/namtor.
fn scratch_state() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("emitter-state.toml");
    std::env::set_var("NAMTOR_EMITTER_STATE", &path);
    (dir, path)
}

// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn teardown_path_end_drop() {
    if !hw_enabled() || std::env::var("NAMTOR_TEARDOWN_HELPER").is_ok() {
        return;
    }
    let _cam = CAMERA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cfg) = reference_cfg() else {
        eprintln!("skipping: no quirks entry for the reference camera");
        return;
    };
    let (_dir, _state) = scratch_state();
    let mut factory = daemon_factory();

    let mut samples = Vec::new();
    for i in 0..SAMPLES {
        let src = factory().unwrap_or_else(|e| panic!("sample {i}: open capture: {e}"));
        assert_armed(&cfg, "after the daemon factory opened a capture");
        let t0 = Instant::now();
        drop(src);
        samples.push(time_until_off(t0, &cfg).unwrap_or_else(|e| panic!("[end] {e}")));
    }
    summarise("end (drop)", &samples);
}

#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn teardown_path_stop() {
    if !hw_enabled() || std::env::var("NAMTOR_TEARDOWN_HELPER").is_ok() {
        return;
    }
    let _cam = CAMERA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cfg) = reference_cfg() else {
        eprintln!("skipping: no quirks entry for the reference camera");
        return;
    };
    let (_dir, _state) = scratch_state();
    let mut factory = daemon_factory();

    let mut samples = Vec::new();
    for i in 0..SAMPLES {
        let mut src = factory().unwrap_or_else(|e| panic!("sample {i}: open capture: {e}"));
        assert_armed(&cfg, "after the daemon factory opened a capture");
        let t0 = Instant::now();
        src.stop();
        let ms = time_until_off(t0, &cfg).unwrap_or_else(|e| panic!("[stop] {e}"));
        // Timed before drop, so this is stop() alone, not stop() plus Drop.
        samples.push(ms);

        // An enrollment session keeps its source across Stop -> Review ->
        // Resume. Darkening the emitter on stop is only correct if Resume
        // lights it again, or the resumed scan would run blind.
        src.start()
            .unwrap_or_else(|e| panic!("sample {i}: start after stop (Resume): {e}"));
        assert_armed(&cfg, "after start() following stop(), i.e. on Resume");
        drop(src);
    }
    summarise("stop", &samples);
}

#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn teardown_path_suspend() {
    if !hw_enabled() || std::env::var("NAMTOR_TEARDOWN_HELPER").is_ok() {
        return;
    }
    let _cam = CAMERA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cfg) = reference_cfg() else {
        eprintln!("skipping: no quirks entry for the reference camera");
        return;
    };
    let (_dir, _state) = scratch_state();

    // A real Daemon with the real capture factory. `DaemonDispatcher::
    // release_for_suspend` locks the dispatcher and calls exactly
    // `Daemon::release`, so this is the suspend path minus the mutex.
    let models = vec![ModelSpec {
        id: "m".into(),
        quant: Quant::Fp32,
        sha256: [0; 32],
    }];
    let mut daemon = Daemon::new(
        MockSupervisor::default(),
        &models,
        Duration::from_secs(30),
        SystemClock,
        Some(daemon_factory()),
    )
    .expect("daemon boot");

    let mut samples = Vec::new();
    for i in 0..SAMPLES {
        daemon
            .prepare_for_auth(Requester {
                uid: 1000,
                label: "pam".into(),
            })
            .unwrap_or_else(|e| panic!("sample {i}: prepare_for_auth: {e}"));
        assert!(daemon.capture_open());
        assert_armed(&cfg, "after prepare_for_auth, i.e. inside the warm window");
        let t0 = Instant::now();
        daemon.release().expect("release is infallible");
        samples.push(time_until_off(t0, &cfg).unwrap_or_else(|e| panic!("[suspend] {e}")));
        assert!(!daemon.capture_open());
    }
    summarise("suspend (Daemon::release)", &samples);
}

/// Not a test on its own: the SIGKILL path re-executes this binary with
/// `NAMTOR_TEARDOWN_HELPER=1` to get a separate process that holds the
/// emitter armed and can be killed without unwinding.
#[test]
#[ignore = "helper process for teardown_path_sigkill"]
fn sigkill_helper_holds_the_emitter() {
    if std::env::var("NAMTOR_TEARDOWN_HELPER").as_deref() != Ok("1") {
        return;
    }
    let mut factory = daemon_factory();
    let src = factory().expect("helper: open capture");
    println!("NAMTOR_ARMED");
    use std::io::Write;
    std::io::stdout().flush().ok();
    // Hold it. SIGKILL ends this; nothing here runs Drop.
    std::thread::sleep(Duration::from_secs(60));
    drop(src);
}

#[test]
#[ignore = "requires the reference camera; run with NAMTOR_HW_TEST=1"]
fn teardown_path_sigkill() {
    if !hw_enabled() || std::env::var("NAMTOR_TEARDOWN_HELPER").is_ok() {
        return;
    }
    let _cam = CAMERA.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cfg) = reference_cfg() else {
        eprintln!("skipping: no quirks entry for the reference camera");
        return;
    };
    let (_dir, state) = scratch_state();
    let exe = std::env::current_exe().expect("current_exe");

    let mut samples = Vec::new();
    let mut survived_kill = 0usize;
    for i in 0..SAMPLES {
        let _ = std::fs::remove_file(&state);
        let mut child = Command::new(&exe)
            .args([
                "--exact",
                "sigkill_helper_holds_the_emitter",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("NAMTOR_TEARDOWN_HELPER", "1")
            .env("NAMTOR_EMITTER_STATE", &state)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn helper");

        // Wait for the helper to say it is armed.
        let stdout = child.stdout.take().expect("helper stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.contains("NAMTOR_ARMED") {
                    let _ = tx.send(());
                    return;
                }
            }
        });
        if rx.recv_timeout(Duration::from_secs(20)).is_err() {
            let _ = child.kill();
            panic!("sample {i}: the helper never reported the emitter armed");
        }
        assert_armed(&cfg, "in the helper process before SIGKILL");
        assert!(
            state.exists(),
            "sample {i}: the daemon path must write the emitter record before arming"
        );

        // SIGKILL: no unwinding, no Drop, no chance for the process to clean up.
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL).expect("SIGKILL");
        let _ = child.wait();

        // What the kill left behind. The UVC XU keeps its value across the file
        // handle closing — which is the whole reason the ExecStopPost backstop
        // exists — but this is recorded rather than assumed.
        let after_kill = hw_emitter::read_control(Path::new(IR_NODE), &cfg).unwrap_or_default();
        if holds(&after_kill, &cfg.payload_on) {
            survived_kill += 1;
        }

        // What systemd runs next: ExecStopPost=/usr/bin/namtor emitter-off,
        // with no device argument, reading the record.
        let t0 = Instant::now();
        let code = namtor_camera::emitter_off_cli::main(&[
            "--state".to_string(),
            state.display().to_string(),
        ]);
        let ms = time_until_off(t0, &cfg).unwrap_or_else(|e| panic!("[sigkill] {e}"));
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", std::process::ExitCode::SUCCESS),
            "sample {i}: emitter-off must succeed"
        );
        samples.push(ms);
    }
    eprintln!(
        "G1.5 SIGKILL: the emitter was still armed after the kill in {survived_kill}/{SAMPLES} \
         samples — the backstop is what turned it off"
    );
    summarise("SIGKILL (ExecStopPost backstop)", &samples);
}
