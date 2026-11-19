// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::daemon::CaptureFactory;
use crate::emitter_cycle::{
    CycleAction, CyclePhase, DutyCycler, DEFAULT_COOLDOWN_MS, DEFAULT_MAX_ON_MS,
};
use crate::lifecycle::{Clock, SystemClock};
use crate::DaemonError;
use namtor_camera::always_on::ProtectionLevel;
use namtor_camera::emitter_ctrl::{ApplyOutcome, EmitterController, EnvConsent};
use namtor_camera::emitter_state::EmitterState;
use namtor_camera::hotplug::{DeviceEvent, HotplugState};
use namtor_camera::inventory::{
    select_camera_from_inventory, CameraInventory, DiscoveredCamera, SysfsInventory,
};
use namtor_camera::resume::{ReconcileOutcome, Reconciler, SleepEvent};
use namtor_camera::watchdog::force_off_lib;
use namtor_capture::ir_uvc::IrUvc;
use namtor_capture::source::CaptureSource;
use namtor_hwdb::quirks::EmitterCfg;
use namtor_uvc::probe::{SafeXu, XuTransport};

/// Keeps "another program has the camera" typed on its way out of the
/// capture layer. Everything else flattens to [`DaemonError::Capture`].
fn capture_err(e: namtor_capture::source::CaptureError) -> crate::DaemonError {
    match e {
        namtor_capture::source::CaptureError::Busy(detail) => {
            crate::DaemonError::CameraBusy(detail)
        }
        other => crate::DaemonError::Capture(other.to_string()),
    }
}

pub fn factory_with_configured(configured: Option<String>) -> CaptureFactory {
    factory_with_inventory(Box::new(SysfsInventory::default()), configured, None)
}

pub fn factory_with_inventory(
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
) -> CaptureFactory {
    factory_with_inventory_at(
        inventory,
        configured,
        enrolled,
        PathBuf::from(namtor_camera::emitter_state::DEFAULT_PATH),
    )
}

/// As [`factory_with_inventory`], with the emitter-state record written to an
/// explicit path.
///
/// The path is a parameter rather than an environment variable on purpose.
/// An env override would let anything able to set the daemon's environment
/// redirect the record, so `ExecStopPost=/usr/bin/namtor emitter-off` would
/// find nothing at the default path, take its "nothing was armed" branch and
/// exit 0 — silently disarming the SIGKILL backstop. That is the same threat
/// model that removed `NAMTOR_GROUP_FILE` from this daemon, and it gets the
/// same answer.
pub fn factory_with_inventory_at(
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
    state_path: PathBuf,
) -> CaptureFactory {
    Box::new(move || {
        let cameras = inventory.enumerate().map_err(crate::DaemonError::Capture)?;
        let selected =
            select_camera_from_inventory(&cameras, configured.as_deref(), enrolled.as_deref())
                .ok_or_else(|| {
                    crate::DaemonError::Capture("no IR camera found via topology selection".into())
                })?;
        open_selected_camera(&selected.camera, &state_path)
    })
}

fn open_selected_camera(
    cam: &DiscoveredCamera,
    state_path: &Path,
) -> Result<Box<dyn CaptureSource>, crate::DaemonError> {
    let dev = &cam.ir_node.dev_path;
    let meta_dev = cam.metadata_node.as_ref().map(|n| &n.dev_path);

    let emitter_cfg = cam.quirks_entry.as_ref().map(|e| &e.emitter);
    let emitter = activate_emitter(dev, emitter_cfg, state_path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, dev = %dev.display(), "emitter activation failed");
        EmitterGuard::failed(emitter_cfg.cloned())
    });
    tracing::info!(protection = ?emitter.protection_level(), "emitter ready");

    let streamer = namtor_capture::ir_uvc::V4l2IrStream::open(
        dev,
        meta_dev.map(|p| p.as_path()),
        cam.width,
        cam.height,
    )
    .map_err(capture_err)?;
    let caps = namtor_capture::source::SourceCaps {
        spectrum: namtor_capture::source::Spectrum::Ir,
        format: namtor_capture::frame::PixFmt::Grey8,
        width: cam.width,
        height: cam.height,
        fps: cam.fps,
        has_metadata: meta_dev.is_some(),
    };
    let mut source =
        namtor_capture::ir_uvc::IrUvc::new(streamer, namtor_capture::ir_uvc::ParityPolicy, caps);
    source.start().map_err(capture_err)?;
    Ok(Box::new(EmitterSource::new(source, emitter)) as Box<dyn CaptureSource>)
}

/// Owns the IR emitter for one open capture and duty-cycles it (spec
/// §4.10.2, issue #30).
///
/// The emitter is on in bounded windows of at most `max_on_ms` (default
/// 2000, from quirks), separated by a cooldown of [`DEFAULT_COOLDOWN_MS`].
/// Two enforcement paths share one serialized state ([`Shared`]):
///
/// - **Frame demand** (`duty_tick` before every pull, `rearm` on start,
///   `ensure_on` on verify entry) runs the full duty cycle: expiry forces
///   off, cooldown elapses, re-arm happens *only here* — the background
///   never re-arms without active frame demand.
/// - **The off-only watchdog thread** exists for the phases where no pull
///   ever comes (the 30 s warm window, an indefinitely blocked
///   `next_frame`): it waits out the on-budget deadline and performs the
///   force-off itself. It can only ever darken; it exits and is joined on
///   drop. Deadlines are exact; a [`WATCHDOG_POLL`] wait bounds how far a
///   wake can overshoot (plus normal scheduler/ioctl latency) — no unbounded
///   on state, no hard-realtime promise.
///
/// Failure semantics:
///
/// - A force-off write that fails leaves the phase `On`, so *both* paths
///   keep retrying — the watchdog at poll granularity, the frame path per
///   pull — and `Drop` is the final backstop.
/// - A turn-on write that fails fails closed: the emitter stays dark and
///   retries are paced by the cooldown (re-anchored on each failure), so
///   verification sees dark frames rather than mislabelled ones.
/// - Transition waits (`ensure_on`/`rearm`) wait out the *full* cooldown,
///   bounded by [`TRANSITION_WAIT_CAP`]; on a misbehaving clock the emitter
///   stays dark rather than skipping the cooldown.
struct EmitterGuard<T: XuTransport + Send + 'static> {
    shared: SharedLock<T>,
    watchdog: Option<JoinHandle<()>>,
}

/// Watchdog poll granularity. The on-budget deadline itself is exact; this
/// bounds only how long a wake can overshoot it (plus normal scheduler and
/// ioctl latency). Not a hard-realtime promise.
const WATCHDOG_POLL: Duration = Duration::from_millis(50);

/// Hard cap on the bounded cooldown wait in the verify/start transition
/// paths. The cooldown is 300 ms by default; the cap trips only if the clock
/// misbehaves (e.g. a mock clock nobody advances), in which case the emitter
/// stays dark — fail closed, never skip the cooldown.
const TRANSITION_WAIT_CAP: Duration = Duration::from_secs(1);

/// MMAP ring depth used by `IrUvc::start` (4 buffers). After a control
/// transition every queued buffer may hold a frame captured before the write
/// landed.
const RING_DEPTH: usize = 4;
/// Frames discarded after each emitter control transition: one full ring
/// drain, so no pre-write frame reaches the pairing path.
const TRANSITION_DISCARD_FRAMES: usize = RING_DEPTH - 1;

type SharedLock<T> = std::sync::Arc<(std::sync::Mutex<Shared<T>>, std::sync::Condvar)>;

/// The serialized emitter control state, shared between the frame-demand
/// path (which owns `&mut` via the source) and the watchdog thread.
struct Shared<T: XuTransport> {
    xu: Option<Box<SafeXu<T>>>,
    cfg: Option<EmitterCfg>,
    outcome: ApplyOutcome,
    cycle: DutyCycler,
    /// Flood control for repeated failed control writes: warn on the first
    /// failure of an episode, debug afterwards; reset on any success.
    write_failure_warned: bool,
    /// A turn-on write that reported failure may nonetheless have lit the
    /// device (a control transfer can fail after the value landed). When the
    /// follow-up force-off also fails, the device state is unknown-but-
    /// possibly-lit; the watchdog then enforces it like an on-window until a
    /// force-off succeeds. The frame-demand path never re-arms out of it
    /// without going through a turn-on (whose `apply_on_gated` force-offs
    /// first), so the cap is honoured throughout.
    unknown_lit: bool,
    /// Monotonic count of successful control transitions (turn-on or
    /// force-off, from any path). Frame consumers diff it to flush the
    /// buffered frames that predate each write.
    transitions: u64,
    clock: Box<dyn Clock + Send + Sync>,
    poll: Duration,
    shutdown: bool,
}

/// Locks the shared state. A poisoned mutex means a panic happened mid-write;
/// the state may be stale, but continuing is the fail-safe choice: the
/// watchdog keeps enforcing the cap and `Drop` still darkens.
fn lock_shared<T: XuTransport>(shared: &SharedLock<T>) -> std::sync::MutexGuard<'_, Shared<T>> {
    shared
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<T: XuTransport> Shared<T> {
    fn now(&self) -> Instant {
        self.clock.now()
    }

    fn activatable(&self) -> bool {
        self.outcome == ApplyOutcome::Activated && self.xu.is_some() && self.cfg.is_some()
    }

    /// Log a failed control write: warn on the first failure of an episode,
    /// debug afterwards (a retry loop must not flood the journal).
    fn log_write_failure(&mut self, msg: &str, e: String) {
        if self.write_failure_warned {
            tracing::debug!(error = %e, "{msg}");
        } else {
            tracing::warn!(error = %e, "{msg}");
            self.write_failure_warned = true;
        }
    }

    /// Attempt to turn the emitter on. On success, records the fresh
    /// on-budget and a transition. Returns whether the device is now driven on.
    fn turn_on(&mut self) -> bool {
        if !self.activatable() {
            return false;
        }
        let outcome = {
            let (Some(xu), Some(cfg)) = (self.xu.as_mut(), self.cfg.as_ref()) else {
                return false;
            };
            let mut ctrl = EmitterController::new(xu.as_mut(), cfg);
            match ctrl.apply_on_gated(&EnvConsent) {
                Ok(ApplyOutcome::Activated) => {
                    ctrl.release_to_guard();
                    Ok(ApplyOutcome::Activated)
                }
                other => other,
            }
        };
        match outcome {
            Ok(ApplyOutcome::Activated) => {
                self.cycle.activate(self.now());
                self.transitions += 1;
                self.write_failure_warned = false;
                self.unknown_lit = false;
                true
            }
            Ok(_) => false,
            Err(e) => {
                self.log_write_failure(
                    "emitter turn-on write failed; staying dark (fail closed)",
                    e.to_string(),
                );
                // The failed write may nonetheless have lit the device (a
                // control transfer can fail after the value landed): darken
                // immediately so the state is known again.
                if !self.darken() {
                    self.unknown_lit = true;
                }
                false
            }
        }
    }

    /// Attempt to force the emitter off. On success, records the cooldown and
    /// a transition. Returns whether the force-off write succeeded.
    fn darken(&mut self) -> bool {
        let (Some(xu), Some(cfg)) = (self.xu.as_mut(), self.cfg.as_ref()) else {
            // Unreachable while cycling (phase `On` requires `Activated`,
            // which requires both fields); report success because there is
            // nothing this guard could darken.
            return true;
        };
        match force_off_lib(xu.as_mut(), cfg) {
            Ok(()) => {
                self.cycle.force_off(self.now());
                self.transitions += 1;
                self.write_failure_warned = false;
                self.unknown_lit = false;
                true
            }
            Err(e) => {
                self.log_write_failure(
                    "emitter force-off write failed; the emitter may still be lit and the \
                     watchdog keeps retrying",
                    e,
                );
                false
            }
        }
    }

    /// On-budget expiry (watchdog or frame path): force off, log the
    /// transition with the elapsed on-time.
    fn expire_and_log(&mut self) {
        let elapsed_ms = self
            .cycle
            .on_elapsed(self.now())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if self.darken() {
            tracing::warn!(
                elapsed_ms = elapsed_ms,
                max_on_ms = self.cycle.max_on_ms(),
                "emitter on-budget expired; forced off (duty cycle, spec §4.10.2)"
            );
        }
    }

    /// Cooldown re-arm (frame path only): turn on, log the transition.
    /// Returns whether the device is now on.
    fn rearm_after_cooldown(&mut self) -> bool {
        if self.turn_on() {
            tracing::info!(
                cooldown_ms = self.cycle.cooldown_ms(),
                "emitter re-armed after duty-cycle cooldown"
            );
            true
        } else {
            false
        }
    }
}

impl<T: XuTransport + Send + 'static> EmitterGuard<T> {
    /// A guard for a device that could not be activated. Never cycles and
    /// needs no watchdog: the emitter was never ours to control.
    fn failed(cfg: Option<EmitterCfg>) -> Self {
        Self {
            shared: std::sync::Arc::new((
                std::sync::Mutex::new(Shared {
                    xu: None,
                    cfg,
                    outcome: ApplyOutcome::SkippedAlwaysOn,
                    cycle: DutyCycler::new(DEFAULT_MAX_ON_MS, DEFAULT_COOLDOWN_MS),
                    write_failure_warned: false,
                    unknown_lit: false,
                    transitions: 0,
                    clock: Box::new(SystemClock),
                    poll: WATCHDOG_POLL,
                    shutdown: false,
                }),
                std::sync::Condvar::new(),
            )),
            watchdog: None,
        }
    }

    /// A guard for a device whose initial gated activation has already run
    /// (`outcome`). When activated, the first on-window starts now and the
    /// off-only watchdog thread is spawned.
    fn new(
        xu: Box<SafeXu<T>>,
        cfg: Option<EmitterCfg>,
        outcome: ApplyOutcome,
        max_on_ms: u32,
        cooldown_ms: u32,
        clock: Box<dyn Clock + Send + Sync>,
        poll: Duration,
    ) -> Self {
        let mut state = Shared {
            xu: Some(xu),
            cfg,
            outcome,
            cycle: DutyCycler::new(max_on_ms, cooldown_ms),
            write_failure_warned: false,
            unknown_lit: false,
            transitions: 0,
            clock,
            poll,
            shutdown: false,
        };
        if outcome == ApplyOutcome::Activated {
            state.cycle.activate(state.now());
        }
        let shared: SharedLock<T> =
            std::sync::Arc::new((std::sync::Mutex::new(state), std::sync::Condvar::new()));
        let watchdog = if outcome == ApplyOutcome::Activated {
            let thread_shared = shared.clone();
            std::thread::Builder::new()
                .name("emitter-watchdog".into())
                .spawn(move || watchdog_loop(thread_shared))
                .ok()
        } else {
            None
        };
        Self { shared, watchdog }
    }

    fn protection_level(&self) -> ProtectionLevel {
        match self.lock().outcome {
            ApplyOutcome::Activated => ProtectionLevel::Full,
            ApplyOutcome::SkippedVendorXuNoConsent | ApplyOutcome::SkippedAlwaysOn => {
                ProtectionLevel::Reduced
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared<T>> {
        lock_shared(&self.shared)
    }

    /// Frame path: run the duty-cycle machine. Returns the transitions
    /// counter for the caller's buffered-frame flush.
    fn duty_tick(&self) -> u64 {
        let cvar = &self.shared.1;
        let mut s = lock_shared(&self.shared);
        let now = s.now();
        match s.cycle.tick(now) {
            CycleAction::None => {}
            CycleAction::Expire => {
                s.expire_and_log();
                cvar.notify_all();
            }
            CycleAction::Rearm => {
                // L1: a failed turn-on re-anchors the cooldown so the next
                // retry is paced by the cooldown, not the frame rate.
                if !s.rearm_after_cooldown() {
                    let now = s.now();
                    s.cycle.enter_cooldown(now);
                }
                cvar.notify_all();
            }
        }
        s.transitions
    }

    /// Start path (resume after stop): wait out any remaining cooldown
    /// (bounded), then re-arm with a fresh budget. A no-op while already on
    /// (e.g. a stop whose force-off failed): the stale budget expires on the
    /// first tick instead.
    fn rearm(&self) -> u64 {
        let cvar = &self.shared.1;
        if !self.wait_for_rearm_allowed(cvar) {
            return self.lock().transitions;
        }
        let mut s = lock_shared(&self.shared);
        if s.outcome != ApplyOutcome::Activated || s.cycle.phase() == CyclePhase::On {
            return s.transitions;
        }
        if !s.rearm_after_cooldown() {
            let now = s.now();
            s.cycle.enter_cooldown(now);
        }
        cvar.notify_all();
        s.transitions
    }

    /// Verify path: wait out any remaining cooldown (bounded), then turn on
    /// with a fresh budget. A no-op while already on: **the live on-budget is
    /// retained and never resets while the emitter is illuminated**.
    fn ensure_on(&self) -> u64 {
        let cvar = &self.shared.1;
        if !self.wait_for_rearm_allowed(cvar) {
            return self.lock().transitions;
        }
        let mut s = lock_shared(&self.shared);
        if s.cycle.phase() == CyclePhase::On {
            return s.transitions;
        }
        if !s.rearm_after_cooldown() {
            let now = s.now();
            s.cycle.enter_cooldown(now);
        }
        cvar.notify_all();
        s.transitions
    }

    /// Bounded wait until a turn-on is allowed (full cooldown honoured,
    /// including dark time already spent in `Off`). Returns `false` if the
    /// wait hit [`TRANSITION_WAIT_CAP`] — the caller then stays dark rather
    /// than skipping the cooldown.
    fn wait_for_rearm_allowed(&self, cvar: &std::sync::Condvar) -> bool {
        let started = Instant::now();
        loop {
            let wait = {
                let s = self.lock();
                if s.cycle.phase() == CyclePhase::On {
                    return true;
                }
                s.cycle.rearm_allowed_in(s.now())
            };
            if wait.is_zero() {
                return true;
            }
            if started.elapsed() + wait > TRANSITION_WAIT_CAP {
                tracing::warn!(
                    waited_ms = started.elapsed().as_millis() as u64,
                    "emitter transition wait exceeded its cap; staying dark (fail closed)"
                );
                return false;
            }
            let s = self.lock();
            // Wake early on a transition (e.g. the watchdog forcing off
            // changes nothing here, but a concurrent turn-on does) and
            // otherwise re-check at poll granularity.
            let (guard, _) = cvar
                .wait_timeout(s, wait.min(WATCHDOG_POLL))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(guard);
        }
    }

    /// Stop path: darken and fully deactivate. On a failed write the phase
    /// stays `On` — the stream is stopped, so no per-frame tick will run, but
    /// the watchdog keeps retrying the force-off and a later `start` finds
    /// the phase `On`, does not double-arm, and the first tick after start
    /// expires the stale budget. `Drop` is the final backstop.
    fn disarm(&self) {
        let cvar = &self.shared.1;
        let mut s = lock_shared(&self.shared);
        if s.cycle.phase() == CyclePhase::Off {
            return;
        }
        if s.darken() {
            let now = s.now();
            s.cycle.deactivate(now);
            cvar.notify_all();
        }
    }
}

/// The off-only watchdog: enforce the continuous-on cap with no frame demand
/// whatsoever. It can only darken; re-arm belongs exclusively to the
/// frame-demand path. Exits when `shutdown` is set (drop joins it).
fn watchdog_loop<T: XuTransport + Send + 'static>(shared: SharedLock<T>) {
    enum Step {
        Continue,
        Wait(Duration),
    }
    loop {
        let step = {
            let cvar = &shared.1;
            let mut s = lock_shared(&shared);
            if s.shutdown {
                return;
            }
            match s.cycle.phase() {
                CyclePhase::On => {
                    let max_on = Duration::from_millis(u64::from(s.cycle.max_on_ms()));
                    match s.cycle.on_elapsed(s.now()) {
                        Some(elapsed) if elapsed > max_on => {
                            s.expire_and_log();
                            if s.cycle.phase() == CyclePhase::On {
                                // The force-off write failed: pace the retry
                                // instead of hammering the device.
                                Step::Wait(s.poll)
                            } else {
                                cvar.notify_all();
                                Step::Continue
                            }
                        }
                        Some(elapsed) => Step::Wait((max_on - elapsed).min(s.poll)),
                        None => Step::Wait(s.poll),
                    }
                }
                // Cooldown/Off: nothing to enforce — re-arm is never ours.
                // Exception: an unknown-but-possibly-lit state left by a
                // partially-applied failed turn-on whose follow-up force-off
                // also failed. Enforce it like an on-window until a force-off
                // succeeds.
                _ if s.unknown_lit => {
                    if s.darken() {
                        cvar.notify_all();
                        Step::Continue
                    } else {
                        Step::Wait(s.poll)
                    }
                }
                _ => Step::Wait(s.poll),
            }
        };
        match step {
            Step::Continue => {}
            Step::Wait(d) => {
                let guard = lock_shared(&shared);
                let _ = shared.1.wait_timeout(guard, d);
            }
        }
    }
}

impl<T: XuTransport + Send + 'static> Drop for EmitterGuard<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.watchdog.take() {
            {
                let mut s = self.lock();
                s.shutdown = true;
            }
            self.shared.1.notify_all();
            if let Err(e) = handle.join() {
                if e.downcast_ref::<&'static str>().is_some()
                    || e.downcast_ref::<String>().is_some()
                {
                    tracing::warn!(
                        "emitter watchdog thread panicked; per-frame enforcement and Drop remain"
                    );
                }
            }
        }
        let mut s = self.lock();
        if let (Some(mut xu), Some(cfg)) = (s.xu.take(), s.cfg.as_ref()) {
            let _ = force_off_lib(xu.as_mut(), cfg);
        }
    }
}

/// Write the `namtor emitter-off` record for `dev`.
///
/// The device's USB identity is recovered by walking sysfs for the USB device
/// that owns this node, so the backstop can re-resolve the node after a
/// renumber instead of trusting a `/dev/videoN` path that the kernel is free
/// to reassign on the next boot.
fn record_emitter_state(dev: &Path, cfg: &EmitterCfg, state_path: &Path) -> Result<(), String> {
    let unit = cfg.unit.ok_or("emitter config has no unit")?;
    let selector = cfg.selector.ok_or("emitter config has no selector")?;
    if cfg.payload_off.is_empty() {
        return Err("emitter config has an empty payload_off".into());
    }

    let sysfs_root = std::env::var("NAMTOR_SYSFS_ROOT").unwrap_or_else(|_| "/sys".to_string());
    let (vid, pid, interface) = usb_identity_of(Path::new(&sysfs_root), dev)
        .ok_or_else(|| format!("no USB device in sysfs owns {}", dev.display()))?;

    let state = EmitterState {
        vid,
        pid,
        interface,
        device: dev.to_path_buf(),
        unit,
        selector,
        payload_off: cfg.payload_off.clone(),
    };
    state.write(state_path)
}

/// Find the USB vendor/product and UVC interface number that own `node`.
fn usb_identity_of(sysfs_root: &Path, node: &Path) -> Option<(u16, u16, u8)> {
    let devices = namtor_hwdb::sysfs::enumerate_usb(sysfs_root).ok()?;
    for usb in &devices {
        let Ok(nodes) = namtor_hwdb::nodes::correlate_nodes(usb) else {
            continue;
        };
        if let Some(n) = nodes.iter().find(|n| n.dev_path == node) {
            return Some((usb.vid, usb.pid, n.interface));
        }
    }
    None
}

fn activate_emitter(
    dev: &Path,
    emitter_cfg: Option<&EmitterCfg>,
    state_path: &Path,
) -> Result<EmitterGuard<namtor_uvc::device::V4l2Device>, String> {
    use namtor_uvc::device::V4l2Device;

    let cfg = emitter_cfg.ok_or_else(|| "no emitter config for selected device".to_string())?;

    // Record how to darken this emitter BEFORE turning it on, so the record
    // exists even if we die between here and the first frame. `ExecStopPost`
    // reads it after SIGKILL, when nothing can be asked of this process. A
    // failure to write is logged, not fatal: refusing to authenticate because
    // a backstop file could not be written would be the worse trade.
    if let Err(e) = record_emitter_state(dev, cfg, state_path) {
        tracing::warn!(
            error = %e,
            "could not record emitter state; the ExecStopPost backstop will have nothing to read"
        );
    }

    let v4l2_dev = V4l2Device::open(dev).map_err(|e| format!("open {}: {e}", dev.display()))?;
    let mut xu = Box::new(SafeXu::new(v4l2_dev));
    let max_on_ms = cfg
        .lifecycle
        .as_ref()
        .map(|l| l.max_on_ms)
        .unwrap_or(DEFAULT_MAX_ON_MS);

    let outcome = {
        let mut ctrl = EmitterController::new(&mut xu, cfg);
        let r = ctrl.apply_on_gated(&EnvConsent);
        match r {
            Ok(o) => {
                if o == ApplyOutcome::Activated {
                    ctrl.release_to_guard();
                }
                o
            }
            Err(e) => return Err(format!("emitter gated apply failed: {e}")),
        }
    };

    Ok(EmitterGuard::new(
        xu,
        emitter_cfg.cloned(),
        outcome,
        max_on_ms,
        DEFAULT_COOLDOWN_MS,
        Box::new(SystemClock),
        WATCHDOG_POLL,
    ))
}

struct EmitterSource<
    S: namtor_capture::ir_uvc::Streamer,
    P: namtor_capture::ir_uvc::EmitterPolicy,
    T: XuTransport + Send + 'static,
> {
    source: namtor_capture::ir_uvc::IrUvc<S, P>,
    emitter: EmitterGuard<T>,
    /// Last seen [`Shared::transitions`] value; each new transition (from any
    /// path, including the watchdog) schedules a ring flush.
    seen_transitions: u64,
    /// Buffered frames to discard before handing out the next real frame.
    pending_discards: usize,
}

impl<
        S: namtor_capture::ir_uvc::Streamer,
        P: namtor_capture::ir_uvc::EmitterPolicy,
        T: XuTransport + Send + 'static,
    > EmitterSource<S, P, T>
{
    fn new(source: IrUvc<S, P>, emitter: EmitterGuard<T>) -> Self {
        Self {
            source,
            emitter,
            seen_transitions: 0,
            pending_discards: 0,
        }
    }

    /// Schedule a full ring drain for every transition that happened since
    /// the last pull, whichever path made it.
    fn absorb_transitions(&mut self, transitions: u64) {
        let delta = transitions.saturating_sub(self.seen_transitions);
        self.seen_transitions = transitions;
        self.pending_discards = self
            .pending_discards
            .saturating_add(delta as usize * TRANSITION_DISCARD_FRAMES);
    }
}

impl<
        S: namtor_capture::ir_uvc::Streamer,
        P: namtor_capture::ir_uvc::EmitterPolicy,
        T: XuTransport + Send + 'static,
    > CaptureSource for EmitterSource<S, P, T>
{
    fn caps(&self) -> namtor_capture::source::SourceCaps {
        self.source.caps()
    }

    fn start(&mut self) -> Result<(), namtor_capture::source::CaptureError> {
        // Stream first, emitter second. `source.start()` can fail (REQBUFS,
        // mmap, STREAMON); arming before it meant a failed start returned Err
        // with the emitter lit and no stream, so `next_frame` — the only thing
        // that runs the duty cycle — would never be called.
        self.source.start()?;
        // Resume after a Stop: `stop` darkened the emitter, so light it again
        // now that frames are actually coming. A no-op on first start; waits
        // out any remaining cooldown, bounded.
        let transitions = self.emitter.rearm();
        self.absorb_transitions(transitions);
        Ok(())
    }

    fn next_frame(
        &mut self,
    ) -> Result<Option<namtor_capture::frame::Frame>, namtor_capture::source::CaptureError> {
        let transitions = self.emitter.duty_tick();
        self.absorb_transitions(transitions);
        // A control write (duty-cycle expiry, cooldown re-arm, verify entry)
        // invalidates the frames queued in the V4L2 ring before it landed:
        // drain the ring before handing out a real frame. The transition may
        // have been made by the watchdog while no pull was running.
        while self.pending_discards > 0 {
            self.pending_discards -= 1;
            self.source.next_frame()?;
        }
        self.source.next_frame()
    }

    fn stop(&mut self) {
        self.source.stop();
        // A stop must darken the emitter, not only halt the stream: the source
        // may be kept for a later Resume. See `EmitterGuard::disarm`.
        self.emitter.disarm();
    }

    fn ensure_illuminated(&mut self) {
        let transitions = self.emitter.ensure_on();
        self.absorb_transitions(transitions);
    }
}

/// Opens a discovered camera into a capture source. Production uses
/// [`open_selected_camera`]; tests inject a stub.
type OpenFn = Box<dyn FnMut(&DiscoveredCamera) -> Result<Box<dyn CaptureSource>, String> + Send>;

/// Wires hotplug + suspend/resume reconciliation into the capture lifecycle.
///
/// On device removal of the selected node, or on suspend, the open capture is
/// closed (which forces the emitter off via [`EmitterGuard`]'s `Drop`). On
/// resume or re-add, the inventory is re-enumerated; if the camera renumbered
/// the stale path is invalidated and the new path is selected. A bounded
/// pending-request queue (via [`Reconciler`]) holds auth requests during
/// reconciliation and drops them if the queue overflows.
//
// allow(dead_code): NOT wired into the daemon, and not going to be as-is.
//
// This comment used to call wiring "a separate integration step", which read
// as though suspend and hotplug were handled and merely pending. They were
// not handled at all. The daemon now covers them with less machinery:
//
// - Hotplug and renumbering: both capture factories (`factory_with_*`)
//   re-enumerate sysfs on every open, and the auth capture is opened per PAM
//   request, so the next auth after a replug or a resume finds the camera
//   fresh without a long-lived reconciler.
// - Suspend: `app.rs` subscribes to logind's `PrepareForSleep` under a delay
//   inhibitor and calls `DaemonDispatcher::release_for_suspend` and
//   `Manager3Service::cancel_all_sessions`, which close every capture so the
//   IR emitter is dark before sleep.
//
// What this guard adds beyond that is draining auth requests queued across a
// resume, which the per-request model does not need because PAM clients do not
// queue. It is kept, with its tests, as the design reference for that
// behaviour; if a long-lived capture is ever introduced, this is where to
// start.
#[allow(dead_code)]
pub struct ReconcileGuard {
    hotplug: HotplugState,
    reconciler: Reconciler<fn()>,
    inventory: Box<dyn CameraInventory + Send>,
    configured: Option<String>,
    enrolled: Option<String>,
    open_fn: OpenFn,
    capture: Option<Box<dyn CaptureSource>>,
}

#[allow(dead_code)]
impl ReconcileGuard {
    pub fn new(
        inventory: Box<dyn CameraInventory + Send>,
        configured: Option<String>,
        enrolled: Option<String>,
        open_fn: OpenFn,
    ) -> Self {
        Self {
            hotplug: HotplugState::new(),
            reconciler: Reconciler::new(noop),
            inventory,
            configured,
            enrolled,
            open_fn,
            capture: None,
        }
    }

    pub fn capture_ready(&self) -> bool {
        self.hotplug.capture_ready() && self.capture.is_some()
    }

    pub fn last_outcome(&self) -> Option<&ReconcileOutcome> {
        self.reconciler.last_outcome()
    }

    #[allow(dead_code)]
    pub fn pending_count(&self) -> usize {
        self.reconciler.pending_count()
    }

    #[allow(dead_code)]
    pub fn capture_mut(&mut self) -> Option<&mut Box<dyn CaptureSource>> {
        self.capture.as_mut()
    }

    /// Closes the open capture (if any), which forces the emitter off via
    /// [`EmitterGuard`]'s `Drop`. Used on removal, suspend, and before a
    /// re-open on a renumbered path.
    pub fn close_capture(&mut self) {
        if let Some(mut src) = self.capture.take() {
            src.stop();
        }
    }

    /// Applies a hotplug device event. If the selected node was removed, the
    /// open capture is closed so the emitter is forced off.
    pub fn handle_device_event(&mut self, event: DeviceEvent) {
        let was_ready = self.hotplug.capture_ready();
        self.hotplug.apply(event);
        if was_ready && !self.hotplug.capture_ready() {
            self.close_capture();
        }
    }

    /// Applies a sleep event. On suspend the capture is closed (emitter off).
    /// On resume the inventory is re-enumerated, the camera re-selected
    /// (invalidating a stale path if renumbered), and — if found — re-opened;
    /// queued auth requests are drained for the caller to service. If no
    /// camera is found post-resume, `MissingCamera` is recorded and the
    /// caller must fall back to password.
    pub fn handle_sleep_event(&mut self, event: SleepEvent) {
        match event {
            SleepEvent::Suspend => {
                self.reconciler.handle(SleepEvent::Suspend);
                self.hotplug.handle_sleep(SleepEvent::Suspend);
                self.close_capture();
            }
            SleepEvent::Resume => {
                self.reconciler.handle(SleepEvent::Resume);
                self.hotplug.handle_sleep(SleepEvent::Resume);
                self.reopen_after_resume();
            }
        }
    }

    fn reopen_after_resume(&mut self) {
        let old_path = self.hotplug.selected_path().map(|p| p.to_path_buf());
        let discovered = self.rediscover();
        match discovered {
            Some((cam, path)) => {
                let outcome = match &old_path {
                    Some(old) if *old != path => ReconcileOutcome::StaleNode {
                        old: old.clone(),
                        new: path.clone(),
                    },
                    _ => ReconcileOutcome::Rediscovered { path: path.clone() },
                };
                self.reconciler.record_rediscovery(outcome);
                self.close_capture();
                match (self.open_fn)(&cam) {
                    Ok(src) => {
                        self.hotplug.apply(DeviceEvent::Add(cam.ir_node.clone()));
                        self.hotplug.set_selected(path);
                        self.hotplug.mark_reopened();
                        self.capture = Some(src);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "post-resume re-open failed");
                        self.reconciler
                            .record_rediscovery(ReconcileOutcome::MissingCamera);
                    }
                }
                let _ = self.reconciler.drain_ready();
            }
            None => {
                self.reconciler
                    .record_rediscovery(ReconcileOutcome::MissingCamera);
                let _ = self.reconciler.drain_ready();
            }
        }
    }

    fn rediscover(&self) -> Option<(DiscoveredCamera, PathBuf)> {
        let cameras = self.inventory.enumerate().ok()?;
        let sel = select_camera_from_inventory(
            &cameras,
            self.configured.as_deref(),
            self.enrolled.as_deref(),
        )?;
        Some((sel.camera.clone(), sel.camera.ir_node.dev_path.clone()))
    }

    /// Ensures a capture is open and ready. If the stream was invalidated
    /// (removal/suspend), re-discovers and re-opens. Returns an error if no
    /// camera can be found so the caller can fall back to password.
    pub fn prepare_capture(&mut self) -> Result<(), DaemonError> {
        if self.capture_ready() {
            return Ok(());
        }
        if self.capture.is_some() {
            self.close_capture();
        }
        let (cam, path) = self
            .rediscover()
            .ok_or_else(|| DaemonError::Capture("no IR camera found during reconcile".into()))?;
        let src = (self.open_fn)(&cam).map_err(DaemonError::Capture)?;
        self.hotplug.apply(DeviceEvent::Add(cam.ir_node.clone()));
        self.hotplug.set_selected(path);
        self.hotplug.mark_reopened();
        self.capture = Some(src);
        Ok(())
    }
}

#[allow(dead_code)]
fn noop() {}

#[cfg(test)]
mod tests {
    use super::*;
    use namtor_camera::hotplug::{DeviceEventSource, FakeEventSource};
    use namtor_camera::inventory::FakeInventory;
    use namtor_capture::frame::{Frame, PixFmt};
    use namtor_capture::source::{CaptureError, SourceCaps, Spectrum};
    use namtor_hwdb::nodes::V4l2Node;
    use namtor_hwdb::sysfs::UsbDevice;

    type OpenFn =
        Box<dyn FnMut(&DiscoveredCamera) -> Result<Box<dyn CaptureSource>, String> + Send>;

    struct StubSource {
        stopped: bool,
    }

    impl CaptureSource for StubSource {
        fn caps(&self) -> SourceCaps {
            SourceCaps {
                spectrum: Spectrum::Ir,
                format: PixFmt::Grey8,
                width: 8,
                height: 8,
                fps: 30,
                has_metadata: false,
            }
        }
        fn start(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
        fn next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
            Ok(None)
        }
        fn stop(&mut self) {
            self.stopped = true;
        }
    }

    fn mk_usb() -> UsbDevice {
        UsbDevice {
            sysfs_path: PathBuf::from("/sys/bus/usb/devices/test"),
            vid: 0x3277,
            pid: 0x0018,
            bcd_device: 0x0001,
            descriptors_path: PathBuf::from("/nonexistent"),
        }
    }

    fn mk_node(dev: &str, iface: u8, meta: bool) -> V4l2Node {
        V4l2Node {
            dev_path: PathBuf::from(dev),
            interface: iface,
            is_metadata: meta,
            name: format!("node {dev}"),
        }
    }

    fn mk_camera(dev: &str) -> DiscoveredCamera {
        DiscoveredCamera {
            ir_node: mk_node(dev, 3, false),
            metadata_node: Some(mk_node("/dev/video3", 3, true)),
            rgb_node: None,
            usb: mk_usb(),
            quirks_entry: None,
            width: 640,
            height: 360,
            fps: 30,
            is_internal: true,
        }
    }

    fn open_fn() -> OpenFn {
        Box::new(|_cam| Ok(Box::new(StubSource { stopped: false }) as Box<dyn CaptureSource>))
    }

    fn guard(cameras: Vec<DiscoveredCamera>) -> ReconcileGuard {
        ReconcileGuard::new(Box::new(FakeInventory::new(cameras)), None, None, open_fn())
    }

    #[test]
    fn prepare_capture_opens_from_inventory() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        assert!(!g.capture_ready());
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        assert_eq!(g.hotplug.selected_path(), Some(Path::new("/dev/video2")));
    }

    #[test]
    fn prepare_capture_errors_when_no_camera() {
        let mut g = guard(vec![]);
        let err = g.prepare_capture().unwrap_err();
        assert!(matches!(err, DaemonError::Capture(_)));
        assert!(!g.capture_ready());
    }

    #[test]
    fn remove_of_selected_closes_capture() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        g.handle_device_event(DeviceEvent::Remove(mk_node("/dev/video2", 3, false)));
        assert!(!g.capture_ready());
        assert!(g.capture.is_none(), "capture dropped on removal");
        assert!(g.hotplug.reopen_needed());
    }

    #[test]
    fn remove_of_any_node_invalidates_conservatively() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        g.handle_device_event(DeviceEvent::Remove(mk_node("/dev/video9", 0, false)));
        assert!(
            !g.capture_ready(),
            "any remove invalidates — the selected node cannot be distinguished from a sibling in udev ordering"
        );
        assert!(g.capture.is_none(), "capture closed on any remove");
        assert!(g.hotplug.reopen_needed());
    }

    #[test]
    fn fake_event_remove_add_remove_add_cycles() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        let mut src = FakeEventSource::new(vec![
            DeviceEvent::Remove(mk_node("/dev/video2", 3, false)),
            DeviceEvent::Add(mk_node("/dev/video2", 3, false)),
            DeviceEvent::Remove(mk_node("/dev/video2", 3, false)),
            DeviceEvent::Add(mk_node("/dev/video2", 3, false)),
        ]);
        while let Some(ev) = src.next_event() {
            g.handle_device_event(ev);
        }
        assert!(
            g.hotplug.reopen_needed(),
            "Add does not clear reopen_needed; only mark_reopened does"
        );
        assert!(!g.capture_ready());
        g.prepare_capture().unwrap();
        assert!(g.capture_ready());
        assert!(!g.hotplug.reopen_needed());
    }

    #[test]
    fn suspend_closes_capture_and_resume_reopens_same_path() {
        let mut g = guard(vec![mk_camera("/dev/video2")]);
        g.prepare_capture().unwrap();
        g.handle_sleep_event(SleepEvent::Suspend);
        assert!(!g.capture_ready());
        assert!(g.capture.is_none(), "suspend closed capture");
        g.handle_sleep_event(SleepEvent::Resume);
        assert!(g.capture_ready(), "resume re-opened");
        assert_eq!(
            g.last_outcome(),
            Some(&ReconcileOutcome::Rediscovered {
                path: "/dev/video2".into()
            })
        );
    }

    #[test]
    fn suspend_then_resume_renumber_records_stale_node() {
        let pre = vec![mk_camera("/dev/video2")];
        let post = vec![mk_camera("/dev/video4")];
        let inv = SwitchingInventory::new(pre, post);
        let mut g = ReconcileGuard::new(Box::new(inv), None, None, open_fn());
        g.prepare_capture().unwrap();
        assert_eq!(g.hotplug.selected_path(), Some(Path::new("/dev/video2")));
        g.handle_sleep_event(SleepEvent::Suspend);
        g.handle_sleep_event(SleepEvent::Resume);
        assert!(g.capture_ready(), "re-opened on renumbered path");
        assert_eq!(
            g.last_outcome(),
            Some(&ReconcileOutcome::StaleNode {
                old: "/dev/video2".into(),
                new: "/dev/video4".into()
            })
        );
        assert_eq!(g.hotplug.selected_path(), Some(Path::new("/dev/video4")));
    }

    #[test]
    fn resume_with_missing_camera_records_missing_and_falls_back() {
        let pre = vec![mk_camera("/dev/video2")];
        let inv = SwitchingInventory::new(pre, vec![]);
        let mut g = ReconcileGuard::new(Box::new(inv), None, None, open_fn());
        g.prepare_capture().unwrap();
        g.handle_sleep_event(SleepEvent::Suspend);
        g.handle_sleep_event(SleepEvent::Resume);
        assert!(!g.capture_ready(), "no camera after resume");
        assert!(g.capture.is_none());
        assert_eq!(g.last_outcome(), Some(&ReconcileOutcome::MissingCamera));
        let err = g.prepare_capture().unwrap_err();
        assert!(matches!(err, DaemonError::Capture(_)));
    }

    /// Inventory that returns `pre` for the first `enumerate` call and `post`
    /// for every call after — simulates a renumber across suspend/resume.
    struct SwitchingInventory {
        pre: Vec<DiscoveredCamera>,
        post: Vec<DiscoveredCamera>,
        first_taken: std::cell::Cell<bool>,
    }

    impl SwitchingInventory {
        fn new(pre: Vec<DiscoveredCamera>, post: Vec<DiscoveredCamera>) -> Self {
            Self {
                pre,
                post,
                first_taken: std::cell::Cell::new(false),
            }
        }
    }

    impl CameraInventory for SwitchingInventory {
        fn enumerate(&self) -> Result<Vec<DiscoveredCamera>, String> {
            if self.first_taken.get() {
                Ok(self.post.clone())
            } else {
                self.first_taken.set(true);
                Ok(self.pre.clone())
            }
        }
    }
}

/// Test-only fakes shared by the emitter tests in this crate (duty-cycle,
/// watchdog, and the daemon-level warm-window cap test).
#[cfg(test)]
pub(crate) mod emitter_test_support {
    use super::*;
    use namtor_uvc::probe::XuError;
    use namtor_uvc::xu::Query;
    use std::sync::{Arc, Mutex};

    pub const PAYLOAD_ON: [u8; 9] = [0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0];
    pub const PAYLOAD_OFF: [u8; 9] = [0x01, 0x03, 0x01, 0, 0, 0, 0, 0, 0];

    pub struct FakeInner {
        pub current: Vec<u8>,
        pub writes: Vec<Vec<u8>>,
        /// Every `SetCur` payload attempted, successful or not — lets tests
        /// observe that a turn-on was *attempted* even when it failed.
        pub attempts: Vec<Vec<u8>>,
        pub fail_sets: bool,
        /// When set, a `SetCur` with this payload is **applied and then
        /// reported as failed** — the model for a control transfer that
        /// errors after the device may have partially taken the value.
        pub fail_after_apply: Option<Vec<u8>>,
        /// When true, a `SetCur` with the off payload fails (and does not
        /// apply).
        pub fail_off: bool,
    }

    /// Fake XU transport with one control at unit 4, selector 6. Clones share
    /// the device state, so the test keeps a handle while the source owns one.
    #[derive(Clone)]
    pub struct FakeDev {
        pub(crate) inner: Arc<Mutex<FakeInner>>,
    }

    impl FakeDev {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(FakeInner {
                    current: PAYLOAD_OFF.to_vec(),
                    writes: Vec::new(),
                    attempts: Vec::new(),
                    fail_sets: false,
                    fail_after_apply: None,
                    fail_off: false,
                })),
            }
        }

        pub fn set_fail_sets(&self, fail: bool) {
            self.inner.lock().unwrap().fail_sets = fail;
        }

        /// Model a control transfer that errors after the device may have
        /// taken the value.
        pub fn set_fail_after_apply(&self, payload: Option<Vec<u8>>) {
            self.inner.lock().unwrap().fail_after_apply = payload;
        }

        pub fn set_fail_off(&self, fail: bool) {
            self.inner.lock().unwrap().fail_off = fail;
        }

        /// Number of `SetCur` attempts with the on payload (any outcome).
        pub fn on_attempts(&self) -> usize {
            self.inner
                .lock()
                .unwrap()
                .attempts
                .iter()
                .filter(|w| w.as_slice() == PAYLOAD_ON)
                .count()
        }

        pub fn current(&self) -> Vec<u8> {
            self.inner.lock().unwrap().current.clone()
        }

        /// Number of times `payload_on` was written.
        pub fn on_writes(&self) -> usize {
            self.inner
                .lock()
                .unwrap()
                .writes
                .iter()
                .filter(|w| w.as_slice() == PAYLOAD_ON)
                .count()
        }
    }

    impl XuTransport for FakeDev {
        fn query(
            &self,
            unit: u8,
            selector: u8,
            query: Query,
            buf: &mut [u8],
        ) -> Result<(), XuError> {
            if (unit, selector) != (4, 6) {
                return Err(XuError::Ioctl("ENOENT".into()));
            }
            let mut inner = self.inner.lock().unwrap();
            match query {
                Query::GetLen => {
                    buf[0] = 9;
                }
                Query::GetInfo => buf[0] = 0b11,
                Query::GetCur => buf.copy_from_slice(&inner.current),
                Query::SetCur => {
                    inner.attempts.push(buf.to_vec());
                    if inner.fail_sets {
                        return Err(XuError::Ioctl("EIO".into()));
                    }
                    if inner.fail_off && buf == PAYLOAD_OFF {
                        return Err(XuError::Ioctl("EIO-off".into()));
                    }
                    if inner.fail_after_apply.as_deref() == Some(&buf[..]) {
                        // Apply, then report failure: the device state is now
                        // ambiguous from the writer's point of view.
                        inner.current = buf.to_vec();
                        return Err(XuError::Ioctl("EIO-partial".into()));
                    }
                    inner.current = buf.to_vec();
                    inner.writes.push(buf.to_vec());
                }
                _ => return Err(XuError::Ioctl("unsupported".into())),
            }
            Ok(())
        }
    }

    pub fn msxu_cfg(max_on_ms: u32) -> EmitterCfg {
        EmitterCfg {
            method: namtor_hwdb::quirks::EmitterMethod::MsxuMode,
            guid: Some("0f3f95dc-2632-4c4e-92c9-a04782f43bc8".into()),
            vc_interface: Some(2),
            unit: Some(4),
            selector: Some(6),
            length: Some(9),
            payload_on: PAYLOAD_ON.to_vec(),
            payload_off: PAYLOAD_OFF.to_vec(),
            mode: Some(2),
            strobe: None,
            idle: None,
            lifecycle: Some(namtor_hwdb::quirks::LifecycleCfg { max_on_ms }),
        }
    }

    /// A production-shaped `EmitterSource` over the fake device, armed like
    /// `activate_emitter` does it. For emitter tests across this crate.
    pub(crate) fn armed_test_source(
        dev: &FakeDev,
        max_on_ms: u32,
        cooldown_ms: u32,
        clock: Box<dyn Clock + Send + Sync>,
        poll: Duration,
    ) -> Box<dyn CaptureSource> {
        let mut xu = Box::new(SafeXu::new(dev.clone()));
        let cfg = msxu_cfg(max_on_ms);
        let outcome = {
            let mut ctrl = EmitterController::new(xu.as_mut(), &cfg);
            match ctrl.apply_on_gated(&EnvConsent) {
                Ok(ApplyOutcome::Activated) => {
                    ctrl.release_to_guard();
                    ApplyOutcome::Activated
                }
                Ok(other) => other,
                Err(e) => panic!("test arming failed: {e}"),
            }
        };
        let guard = EmitterGuard::new(xu, Some(cfg), outcome, max_on_ms, cooldown_ms, clock, poll);
        let caps = namtor_capture::source::SourceCaps {
            spectrum: namtor_capture::source::Spectrum::Ir,
            format: namtor_capture::frame::PixFmt::Grey8,
            width: 64,
            height: 64,
            fps: 30,
            has_metadata: false,
        };
        let streamer = EndlessTestStreamer;
        let src = IrUvc::new(streamer, namtor_capture::ir_uvc::ParityPolicy, caps);
        Box::new(EmitterSource::new(src, guard))
    }

    /// A streamer that always has the next frame (never errors, never ends).
    pub(crate) struct EndlessTestStreamer;

    impl namtor_capture::ir_uvc::Streamer for EndlessTestStreamer {
        fn next_raw(
            &mut self,
        ) -> Result<namtor_capture::ir_uvc::RawFrame, namtor_capture::source::CaptureError>
        {
            Ok(namtor_capture::ir_uvc::RawFrame {
                data: vec![0u8; 4096],
                ts: std::time::Duration::ZERO,
                sof: None,
                frame_index: 0,
            })
        }
    }
}

/// Deterministic regressions for the emitter duty cycle (issue #30).
///
/// Everything runs on [`MockClock`] and a fake XU transport: no sleeping, no
/// device. The device state (`FakeDev::current`) is the source of truth —
/// every "emitter is on/off" assertion reads the control back.
#[cfg(test)]
mod emitter_duty_cycle_tests {
    use super::emitter_test_support::{FakeDev, PAYLOAD_OFF, PAYLOAD_ON};
    use super::*;
    use crate::lifecycle::MockClock;
    use namtor_capture::frame::PixFmt;
    use namtor_capture::ir_uvc::{RawFrame, Streamer};
    use namtor_capture::source::{CaptureError, SourceCaps, Spectrum};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Watchdog poll so large that the background thread stays dormant for
    /// the whole mock test (its waits are mock-clock-based); drop notifies it
    /// to exit. All enforcement in this module is exercised on the frame path.
    const DORMANT_POLL: Duration = Duration::from_secs(60);

    /// A streamer that always has the next frame, counting raw pulls through a
    /// shared counter so the test can compare pulls with frames handed out.
    struct EndlessStreamer {
        n: Arc<AtomicU64>,
    }

    impl EndlessStreamer {
        fn new() -> (Self, Arc<AtomicU64>) {
            let n = Arc::new(AtomicU64::new(0));
            (Self { n: n.clone() }, n)
        }
    }

    impl Streamer for EndlessStreamer {
        fn next_raw(&mut self) -> Result<RawFrame, CaptureError> {
            let i = self.n.fetch_add(1, Ordering::SeqCst);
            Ok(RawFrame {
                data: vec![0u8; 4096],
                ts: Duration::from_millis(33 * i),
                sof: None,
                frame_index: i,
            })
        }
    }

    /// Mirrors production `activate_emitter`: apply the on payload first, then
    /// hand the transport to a guard whose cycle is anchored at `now`.
    /// Returns the source and the streamer's raw-pull counter.
    fn armed_source(
        dev: &FakeDev,
        clock: MockClock,
    ) -> (
        EmitterSource<EndlessStreamer, namtor_capture::ir_uvc::ParityPolicy, FakeDev>,
        Arc<AtomicU64>,
    ) {
        let mut xu = Box::new(SafeXu::new(dev.clone()));
        let cfg = emitter_test_support::msxu_cfg(2000);
        let outcome = {
            let mut ctrl = EmitterController::new(xu.as_mut(), &cfg);
            match ctrl.apply_on_gated(&EnvConsent) {
                Ok(ApplyOutcome::Activated) => {
                    ctrl.release_to_guard();
                    ApplyOutcome::Activated
                }
                Ok(other) => other,
                Err(e) => panic!("test arming failed: {e}"),
            }
        };
        let guard = EmitterGuard::new(
            xu,
            Some(cfg),
            outcome,
            2000,
            DEFAULT_COOLDOWN_MS,
            Box::new(clock.clone()),
            DORMANT_POLL,
        );
        let caps = SourceCaps {
            spectrum: Spectrum::Ir,
            format: PixFmt::Grey8,
            width: 64,
            height: 64,
            fps: 30,
            has_metadata: false,
        };
        let (streamer, raws) = EndlessStreamer::new();
        let src = IrUvc::new(streamer, namtor_capture::ir_uvc::ParityPolicy, caps);
        (EmitterSource::new(src, guard), raws)
    }

    fn ms(m: u64) -> Duration {
        Duration::from_millis(m)
    }

    /// The issue #30 regression: a 10-second stream must duty-cycle. Before
    /// the fix the watchdog forced the emitter off after 2 s and nothing ever
    /// turned it back on — the last 8 s of every capture were dark.
    #[test]
    fn ten_second_stream_stays_illuminated_by_duty_cycling() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        for _ in 0..303 {
            clock.advance(ms(33));
            src.next_frame().unwrap();
        }

        assert!(
            dev.on_writes() >= 4,
            "the emitter must be re-armed after each expiry; got {} on-writes",
            dev.on_writes()
        );
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "the stream must end inside an illuminated window, not permanently dark"
        );
    }

    /// The watchdog expiry is exact: a window of exactly max_on_ms is legal,
    /// one millisecond more is not.
    #[test]
    fn expiry_forces_off_strictly_after_max_on() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        clock.advance(ms(1999));
        src.next_frame().unwrap();
        assert_eq!(dev.current(), PAYLOAD_ON, "1999 ms is within the budget");

        clock.advance(ms(2));
        src.next_frame().unwrap();
        assert_eq!(dev.current(), PAYLOAD_OFF, "2001 ms must expire");
    }

    /// A force-off write that fails must not disarm the watchdog: it retries
    /// on the next frame and darkens as soon as the device accepts a write.
    #[test]
    fn force_off_failure_keeps_retrying_until_it_succeeds() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        dev.set_fail_sets(true);
        clock.advance(ms(2001));
        src.next_frame().unwrap();
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "the write failed, so the emitter is still lit"
        );

        dev.set_fail_sets(false);
        clock.advance(ms(33));
        src.next_frame().unwrap();
        assert_eq!(
            dev.current(),
            PAYLOAD_OFF,
            "the watchdog must retry the force-off instead of disarming"
        );
    }

    /// A turn-on write that fails fails closed (dark) and is retried after
    /// the cooldown, not abandoned for the rest of the session.
    #[test]
    fn turn_on_failure_retries_with_cooldown_pacing() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        clock.advance(ms(2001));
        src.next_frame().unwrap(); // expiry -> off, cooldown starts
        assert_eq!(dev.current(), PAYLOAD_OFF);

        dev.set_fail_sets(true);
        clock.advance(ms(300));
        src.next_frame().unwrap(); // rearm attempted, write fails
        assert_eq!(dev.current(), PAYLOAD_OFF, "failed turn-on stays dark");

        dev.set_fail_sets(false);
        clock.advance(ms(300));
        src.next_frame().unwrap(); // retry succeeds
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "a failed turn-on must be retried after the cooldown"
        );
    }

    /// Stop darkens; resume re-arms with a fresh budget.
    #[test]
    fn stop_darkens_and_resume_rearms_with_fresh_budget() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        clock.advance(ms(1000));
        src.next_frame().unwrap();
        src.stop();
        assert_eq!(dev.current(), PAYLOAD_OFF, "stop must darken the emitter");

        clock.advance(ms(5000));
        src.start().unwrap();
        assert_eq!(dev.current(), PAYLOAD_ON, "resume must re-arm");

        // Fresh budget: expiry is anchored at the resume, not the original t0.
        clock.advance(ms(2001));
        src.next_frame().unwrap();
        assert_eq!(dev.current(), PAYLOAD_OFF);
    }

    /// A stop whose force-off write fails leaves the emitter lit; resuming
    /// must recover the duty cycle instead of going permanently dark.
    #[test]
    fn failed_stop_force_off_recovers_on_resume() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        dev.set_fail_sets(true);
        clock.advance(ms(1500));
        src.stop();
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "the stop force-off failed, so the emitter is still lit"
        );

        dev.set_fail_sets(false);
        // The user sits on the Review screen; resume happens well past the
        // (already exceeded) budget.
        clock.advance(ms(3500));
        src.start().unwrap();
        clock.advance(ms(33));
        src.next_frame().unwrap();
        assert_eq!(
            dev.current(),
            PAYLOAD_OFF,
            "the stale budget expires on the first frame after resume"
        );
        clock.advance(ms(300));
        src.next_frame().unwrap();
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "the duty cycle must resume after the cooldown"
        );
    }

    /// Dropping the guard is the final backstop: the emitter goes dark even
    /// if the stream was never stopped.
    #[test]
    fn drop_forces_the_emitter_off() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();
        assert_eq!(dev.current(), PAYLOAD_ON);
        drop(src);
        assert_eq!(dev.current(), PAYLOAD_OFF, "Drop must darken the emitter");
    }

    /// The verify-readiness hook must not rewrite the control while the
    /// emitter is already on: the live budget is retained and still expires
    /// at its original deadline.
    #[test]
    fn ensure_on_while_on_retains_the_live_budget() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, _raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        clock.advance(ms(1500));
        src.ensure_illuminated();
        assert_eq!(dev.on_writes(), 1, "no re-write while already on");
        clock.advance(ms(501));
        src.next_frame().unwrap();
        assert_eq!(
            dev.current(),
            PAYLOAD_OFF,
            "the budget is still anchored at the original activation"
        );
    }

    /// The verify-readiness hook turns the emitter on only after waiting out
    /// the full cooldown — bounded, never skipped (review round-1 ruling).
    /// Those waits need a real clock; see `emitter_watchdog_tests`
    /// (`ensure_on_waits_out_the_cooldown`,
    /// `ensure_on_write_failure_retries_with_cooldown_pacing`).
    ///
    /// Frames queued in the V4L2 ring before a control write landed were
    /// captured in the previous illumination state. A full ring drain
    /// (`RING_DEPTH - 1` frames) is discarded after each transition, and the
    /// transition may have been made by the watchdog thread while no pull was
    /// running — the counter-based diffing catches both paths.
    #[test]
    fn one_buffered_frame_is_discarded_after_each_control_transition() {
        let dev = FakeDev::new();
        let clock = MockClock::new(Instant::now());
        let (mut src, raws) = armed_source(&dev, clock.clone());
        src.start().unwrap();

        let mut frames = 0u64;
        for _ in 0..121 {
            // ~4 s: exactly one expiry and one cooldown re-arm, i.e. exactly
            // two control transitions.
            clock.advance(ms(33));
            if src.next_frame().unwrap().is_some() {
                frames += 1;
            }
        }

        let raw_pulls = raws.load(Ordering::SeqCst);
        assert_eq!(
            raw_pulls,
            frames + 2 * (RING_DEPTH - 1) as u64,
            "a full ring drain per control transition: the frames captured before \
             the write landed never reach pairing (transitions in 4 s: expiry + re-arm)"
        );
    }
}

/// Real-time regressions for the autonomous off-only watchdog (review B1 of
/// task 2, round 1): the 2 s continuous-on cap must hold with **zero**
/// `next_frame` calls — during the 30 s warm window and while a frame pull is
/// blocked — and the background must never re-arm. Short real deadlines
/// (300 ms) keep these fast; no mock clock here, the clock is real time.
#[cfg(test)]
mod emitter_watchdog_tests {
    use super::emitter_test_support::{armed_test_source, FakeDev, PAYLOAD_OFF, PAYLOAD_ON};
    use super::*;
    use namtor_capture::frame::PixFmt;
    use namtor_capture::ir_uvc::{RawFrame, Streamer};
    use namtor_capture::source::{CaptureError, SourceCaps, Spectrum};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Short real deadline: the cap is enforced with normal scheduler/ioctl
    /// latency, not hard realtime, so a small budget keeps tests fast.
    const MAX_ON_MS: u32 = 300;
    const FAST_POLL: Duration = Duration::from_millis(10);

    /// A streamer whose `next_raw` blocks until released — the model for a
    /// stalled capture / indefinitely blocked frame pull.
    struct BlockingStreamer {
        release: Arc<AtomicBool>,
    }

    impl Streamer for BlockingStreamer {
        fn next_raw(&mut self) -> Result<RawFrame, CaptureError> {
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(RawFrame {
                data: vec![0u8; 4096],
                ts: Duration::ZERO,
                sof: None,
                frame_index: 0,
            })
        }
    }

    /// Real-clock source with the reference 300 ms cooldown and a fast poll.
    fn armed_source_rt(dev: &FakeDev, max_on_ms: u32) -> Box<dyn CaptureSource> {
        armed_test_source(
            dev,
            max_on_ms,
            DEFAULT_COOLDOWN_MS,
            Box::new(SystemClock),
            FAST_POLL,
        )
    }

    /// Polls until `pred` holds, up to `timeout`. Returns whether it held.
    fn wait_for(timeout: Duration, pred: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        pred()
    }

    /// B1: with no frame pull at all — the daemon's Warming phase — the
    /// emitter must be forced off once the continuous-on budget expires.
    #[test]
    fn watchdog_forces_off_during_idle_warming_with_zero_frame_pulls() {
        let dev = FakeDev::new();
        let src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "the on-cap must be enforced with zero next_frame calls (review B1)"
        );
        drop(src);
    }

    /// B1: after the watchdog forces off, the background must NEVER re-arm.
    /// Re-arm happens only on active frame demand (verify/start).
    #[test]
    fn watchdog_never_rearms_in_background() {
        let dev = FakeDev::new();
        let src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: the watchdog must force off (review B1)"
        );
        std::thread::sleep(Duration::from_millis(900));
        assert_eq!(
            dev.current(),
            PAYLOAD_OFF,
            "the background must never re-arm without frame demand"
        );
        assert_eq!(
            dev.on_writes(),
            1,
            "no payload_on write may happen in the background"
        );
        drop(src);
    }

    /// B1: a frame pull blocked indefinitely (stalled capture) must not
    /// extend the on-state: the watchdog darkens while the pull is blocked.
    #[test]
    fn blocked_next_frame_does_not_delay_the_cap() {
        let dev = FakeDev::new();
        let release = Arc::new(AtomicBool::new(false));
        let mut xu = Box::new(SafeXu::new(dev.clone()));
        let cfg = emitter_test_support::msxu_cfg(MAX_ON_MS);
        let outcome = {
            let mut ctrl = EmitterController::new(xu.as_mut(), &cfg);
            match ctrl.apply_on_gated(&EnvConsent) {
                Ok(ApplyOutcome::Activated) => {
                    ctrl.release_to_guard();
                    ApplyOutcome::Activated
                }
                Ok(other) => other,
                Err(e) => panic!("test arming failed: {e}"),
            }
        };
        let guard = EmitterGuard::new(
            xu,
            Some(cfg),
            outcome,
            MAX_ON_MS,
            DEFAULT_COOLDOWN_MS,
            Box::new(SystemClock),
            FAST_POLL,
        );
        let caps = SourceCaps {
            spectrum: Spectrum::Ir,
            format: PixFmt::Grey8,
            width: 64,
            height: 64,
            fps: 30,
            has_metadata: false,
        };
        let mut src = EmitterSource::new(
            IrUvc::new(
                BlockingStreamer {
                    release: release.clone(),
                },
                namtor_capture::ir_uvc::ParityPolicy,
                caps,
            ),
            guard,
        );
        src.start().unwrap();

        let puller = std::thread::spawn(move || {
            let _ = src.next_frame();
        });

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "the cap must hold while next_frame is blocked indefinitely (review B1)"
        );
        release.store(true, Ordering::SeqCst);
        puller.join().unwrap();
    }

    /// The verify-entry transition waits out the full cooldown instead of
    /// skipping it (review round-1 ruling): bounded wait, then a fresh
    /// on-window.
    #[test]
    fn ensure_on_waits_out_the_cooldown() {
        let dev = FakeDev::new();
        let mut src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: watchdog force-off"
        );

        let t = Instant::now();
        src.ensure_illuminated();
        let elapsed = t.elapsed();
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "verify must end up in an illuminated window"
        );
        assert_eq!(dev.on_writes(), 2, "exactly one re-arm write");
        assert!(
            elapsed >= Duration::from_millis(150),
            "the full cooldown must be waited out, not skipped (waited {elapsed:?})"
        );
        assert!(
            elapsed <= Duration::from_secs(2),
            "the wait must be bounded (waited {elapsed:?})"
        );
        drop(src);
    }

    /// The resume transition honours the cooldown too: stop stamps the dark
    /// time, and an immediate start waits out the remainder.
    #[test]
    fn start_waits_out_the_cooldown_after_stop() {
        let dev = FakeDev::new();
        let mut src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: watchdog force-off"
        );

        src.stop();
        assert_eq!(dev.current(), PAYLOAD_OFF);

        let t = Instant::now();
        src.start().unwrap();
        let elapsed = t.elapsed();
        assert_eq!(dev.current(), PAYLOAD_ON, "resume must re-arm");
        assert!(
            elapsed >= Duration::from_millis(150),
            "resume must wait out the remaining cooldown (waited {elapsed:?})"
        );
        assert!(
            elapsed <= Duration::from_secs(2),
            "the wait must be bounded (waited {elapsed:?})"
        );
        drop(src);
    }

    /// A failed ensure_on turn-on fails closed and is retried on later frame
    /// demand with cooldown pacing.
    #[test]
    fn ensure_on_write_failure_retries_with_cooldown_pacing() {
        let dev = FakeDev::new();
        let mut src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: watchdog force-off"
        );

        dev.set_fail_sets(true);
        src.ensure_illuminated(); // waits the cooldown, then the write fails
        assert_eq!(dev.current(), PAYLOAD_OFF, "failed turn-on stays dark");

        dev.set_fail_sets(false);
        // The background never re-arms; the retry happens on frame demand,
        // paced by the cooldown the failed turn-on re-anchored.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !(dev.on_writes() >= 2 && dev.current() == PAYLOAD_ON) {
            let _ = src.next_frame();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            dev.on_writes() >= 2 && dev.current() == PAYLOAD_ON,
            "the failed turn-on must be retried on frame demand after the cooldown"
        );
        drop(src);
    }

    /// A watchdog force-off whose write fails must be retried (fail safe:
    /// keep trying until the device accepts the off payload).
    #[test]
    fn watchdog_retries_force_off_when_writes_fail() {
        let dev = FakeDev::new();
        let src = armed_source_rt(&dev, MAX_ON_MS);
        dev.set_fail_sets(true);

        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            dev.current(),
            PAYLOAD_ON,
            "the write failed, so the emitter is still lit — and must stay retried"
        );

        dev.set_fail_sets(false);
        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "the watchdog must keep retrying the force-off until it succeeds"
        );
        drop(src);
    }

    /// A turn-on write that errors **after the device may have taken the
    /// value** (partial success) must not leave an ambiguous lit state: the
    /// guard force-offs immediately, so the emitter is dark even though the
    /// cycler recorded a failure.
    #[test]
    fn failed_turn_on_with_ambiguous_device_state_is_force_off_retried() {
        let dev = FakeDev::new();
        let mut src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: watchdog force-off (device dark, cooldown running)"
        );

        // From now on, an on-write is applied and then reported as failed:
        // the device may be lit although the write "failed".
        dev.set_fail_after_apply(Some(PAYLOAD_ON.to_vec()));

        // Verify entry: waits out the cooldown, then the turn-on partially
        // applies (device lit, write reported failed). The guard must darken
        // before returning — the emitter may never be left lit-unknown.
        src.ensure_illuminated();
        assert!(
            dev.on_attempts() >= 2,
            "precondition: the turn-on was attempted (initial arm + re-arm)"
        );
        assert_eq!(
            dev.current(),
            PAYLOAD_OFF,
            "an ambiguous failed turn-on must be force-off'd (fail closed)"
        );

        // Once writes work again, frame demand retries the turn-on.
        dev.set_fail_after_apply(None);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !(dev.on_writes() >= 2 && dev.current() == PAYLOAD_ON) {
            let _ = src.next_frame();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            dev.on_writes() >= 2 && dev.current() == PAYLOAD_ON,
            "the failed turn-on must be retried on frame demand after the cooldown"
        );
        drop(src);
    }

    /// If the ambiguous failed turn-on cannot even be darkened (the off
    /// write fails too), the watchdog must treat the unknown state as
    /// lit — keep retrying the force-off until the device accepts it.
    #[test]
    fn unknown_state_after_failed_writes_is_enforced_by_the_watchdog() {
        let dev = FakeDev::new();
        let mut src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);

        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: watchdog force-off"
        );

        // On-writes partially apply; off-writes fail outright.
        dev.set_fail_after_apply(Some(PAYLOAD_ON.to_vec()));
        dev.set_fail_off(true);

        // Frame demand triggers a turn-on that lights the device and reports
        // failure; the follow-up darken fails too -> unknown lit state. Pull
        // until that turn-on was actually attempted (cooldown elapsed).
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && dev.on_attempts() < 2 {
            let _ = src.next_frame();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            dev.on_attempts() >= 2,
            "precondition: the ambiguous turn-on was attempted"
        );

        // Recover the device: now the watchdog must force the unknown state
        // off, even though the cycler believes it is in cooldown (dark).
        dev.set_fail_after_apply(None);
        dev.set_fail_off(false);
        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "an unknown lit state must be enforced by the watchdog (fail closed)"
        );
        drop(src);
    }

    /// Dropping the guard must shut the watchdog down and join it: a dropped
    /// guard's thread may never darken a later guard's window.
    #[test]
    fn drop_joins_the_watchdog_thread() {
        let dev = FakeDev::new();
        let src = armed_source_rt(&dev, MAX_ON_MS);
        assert_eq!(dev.current(), PAYLOAD_ON);
        assert!(
            wait_for(Duration::from_secs(2), || dev.current() == PAYLOAD_OFF),
            "precondition: watchdog force-off"
        );
        drop(src);

        // A fresh guard turns the emitter on; the dropped guard's thread must
        // be gone (joined), or it would see its own stale over-budget window
        // and force the device off again within one poll interval.
        let dev2 = FakeDev::new();
        let src2 = armed_source_rt(&dev2, 60_000);
        assert_eq!(dev2.current(), PAYLOAD_ON);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            dev2.current(),
            PAYLOAD_ON,
            "a joined watchdog can never darken another guard's window"
        );
        drop(src2);
    }
}
