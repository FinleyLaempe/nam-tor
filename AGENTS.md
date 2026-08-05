# Nam-tor — Agent Memory

Project state for the next agent. Read this first, then the spec.

## Current state (2026-08-05)

M0 (eval harness), M1 (hardware spike), M2 (detection + capture), **M3 (daemon + sandboxed worker)** done and committed. Working tree clean on `master`. 297 tests green serial (9 HW-gated `#[ignore]`), clippy/fmt/SPDX clean.

**All 26 M3 tasks committed.** Task 25 (R4 INT8 vs FP32 eval) scaffolding is done — Python script (`ml/recipes/r4_int8_vs_fp32.py`), `#[ignore]` Rust driver (`crates/namtor-daemon/tests/r4.rs`), and report template (`docs/m3-r4-report.md`) all compile and are ready to run. The **actual run is BLOCKED on the human**: Python `onnxruntime` binding broken (`python3 -c "import onnxruntime"` fails despite pacman `onnxruntime-cpu 1.28.0`, likely Python 3.14 incompat), model selection + licence acceptance (`NAMTOR_ACCEPT_LICENCE=1`), and R5 cohort dataset decision. Once those land, run `cargo test -p namtor-daemon --test r4 -- --ignored` then `python3 ml/recipes/r4_int8_vs_fp32.py --fp32 r4_fp32.jsonl --int8 r4_int8.jsonl --out docs/m3-r4-report.md --cohort <name>`.

Everything else in M3 shipped: e2e harness + no-biometric-in-logs assertion (§8.6) green, G5.1 (no ML in proto) structural test green, G5.2 escape suite green, systemd units + sysusers/tmpfiles + setuid helper written.

Next milestone: M4 (enrollment + template storage) once Task 25's R4/R5 results land and M4 plan is written. Do **not** write the M4 plan before R4/R5 land.

## Where things are

| Want | Read |
|---|---|
| The spec (authoritative) | `docs/superpowers/specs/2026-08-04-nam-tor-design.md` |
| Milestone roadmap + global constraints | `docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md` |
| M3 task plan (execute this) | `docs/superpowers/plans/2026-08-04-nam-tor-m3-daemon-worker.md` |
| Architecture map | `docs/architecture.md` |
| Threat model | `docs/threat-model.md` |
| Dataset licences (read before ML data) | `ml/DATASETS.md` |
| Contribution rules + rejected "improvements" | `CONTRIBUTING.md` |
| Licence split | `COPYING.md` |

## M2 shipped API (build on this, don't reimplement)

`namtor_capture::source`: `Spectrum{Ir,Rgb,Depth}`, `SourceCaps{spectrum,format,width,height,fps,has_metadata}`, `CaptureError{NotStarted,Device,Io,Recording}`, `CaptureSource{caps,start,next_frame,stop}` (pull-based; `next_frame → Result<Option<Frame>,CaptureError>`, `Ok(None)` = end-of-stream).
`namtor_capture::frame`: `PixFmt{Grey8,Yuyv,Mjpeg,Depth16}`, `EmitterState{On,Off,Torn,Unknown}`, `Frame`, `Frame::validate()`.
`namtor_capture::session`: `CaptureSession::new(ir: Box<dyn CaptureSource>, rgb: Option<Box<dyn CaptureSource>>)` (RAII Drop), `measure_open_cost(node:&Path) -> Result<Duration,CaptureError>`, `measure_open_cost_with(f: impl FnOnce()) -> Duration`.
`namtor_capture::ir_uvc`/`rgb_uvc`: `IrUvc<S:Streamer,P:EmitterPolicy>`, `RgbUvc<S:Streamer>`, `V4l2IrStream::open`, `V4l2RgbStream::open` (Linux-only, HW `#[ignore]`). **Trait is `Streamer`**; `MockStreamer` is a test impl.
`namtor_camera::emitter_ctrl`: `EmitterController<'a,T:XuTransport>::new(&'a mut SafeXu<T>, &'a EmitterCfg)`, `force_off_before_anything`, `apply_on`, `off`, `verify`, `method`, `is_armed`; `EmitterCtrlError`.
`namtor_camera::watchdog`: `trait Clock`, `SystemClock`, `MockClock::new(Instant)` + `advance(Duration)`, `Watchdog<'a,C:Clock>::new(max_on_ms:u32, &'a C)`, `from_quirks(&EmitterCfg,&'a C)`, `arm`, `disarm`, `is_armed`, `check_expired`; `force_off_lib<T:XuTransport>(&mut SafeXu<T>,&EmitterCfg)`, `force_off_on_device(&Path,&EmitterCfg)`.
`namtor_camera::resume`: `SleepEvent`, `ResumeState`, `ResumeMachine::new()` + `state()` + `handle(event,&dyn SleepSignal)` + `mark_ready()`, `Request`, `PendingState`, `Reconciler<F:FnMut()>::new(on_reconcile)` + `state()` + `pending_count()` + `handle(event)` + `queue_request(req)` + `drain_ready()`.
`namtor_camera::hotplug`: `DeviceEvent`, `trait DeviceEventSource`, `UdevMonitor::new()`, `HotplugState::new()` + `has_device()` + `apply(event)` + `drain(&mut S)`, `FakeEventSource::new(Vec<DeviceEvent>)`.
`namtor_camera::selection`: `SelectReason`, `SelectedDevice`, `select_camera(candidates:&[V4l2Node], configured:Option<&str>, enrolled:Option<&str>, is_internal:impl Fn(&V4l2Node)->bool) -> Option<SelectedDevice>`.
`namtor_camera::always_on`: `detect_always_on(idle_baseline:&[Frame], with_emitter_off:&[Frame]) -> bool`, `ProtectionLevel`.
`namtor_cli::status`: `ProtectionLevel`, `StreamState`, `CameraManager` + `new(..)` + `with_requester(..)` + `from_status_reply(..)`, `run_status(&CameraManager) -> String` (stream open line: `stream: open (requested by uid N "label", warm X.Xs remaining)`).
`namtor_hwdb::sysfs::enumerate_usb(&Path) -> Result<Vec<UsbDevice>,HwdbError>`, `namtor_hwdb::classify::classify_formats(&DescriptorSet) -> Vec<ClassifiedFormat>`, `namtor_hwdb::classify::classify_guid(&[u8;16]) -> FormatKind`, `QuirksDb`, `QuirksEntry`, `EmitterCfg`, `EmitterMethod`.
`namtor_eval::score`: `ScoreRecord`, `LatencyBreakdown`, `write_jsonl`, `read_jsonl`.

## M3 shipped API (build on this, don't reimplement)

`namtor_proto::framing`: `encode(&T, &mut impl Write)`, `decode(&mut impl Read) -> Result<serde_json::Value,ProtoError>` (4-byte BE len + JSON).
`namtor_proto::worker`: `ModelSpec{id,quant,sha256}`, `Quant{Fp32,Int8}`, `FrameRef{offset,len,emitter,spectrum,pix_fmt,w,h,ts_us}`, `WorkerReq{Load(ModelSpec),Infer(FrameRef),Shutdown}`, `WorkerRsp{Loaded(Loaded),Vector{vec:Vec<f32>},Error{msg:String}}`, `Loaded{model_id,quant,input_shape,output_shape}`.
`namtor_proto::pam`: `AuthPhase{Prepare,Verify,Release}`, `AuthBody{nonce,target_uid,phase,rhost,deadline_us}`, `Requester{uid,label}`, `AuthRequest{Auth(AuthBody),Status}` (tag = "kind", snake_case), `AuthReply{nonce,verdict,score,reason}`, `Verdict{Match,NoMatch,Unavailable,RateLimited,Refused}`, `StatusReply{stream,requester,warm_remaining_ms}`, `StreamState{Closed,Warming,Verifying}`, `HelloMsg{server_version,nonce}`.
`namtor_worker::infer`: `InferenceBackend` trait (`load`, `run`), `MockBackend::new(dim)`, `OnnxCpu::new(threads)`, `default_backend()` (feature-gated). `namtor_worker::sandbox::apply_sandbox()`, `seccomp::apply_seccomp()`, `landlock::apply_landlock()`. `namtor_worker::run_main(read_fd, write_fd, backend, apply_sbx, frame_memfd, frame_len)`.
`namtor_daemon::acl`: `PeerCreds{pid,uid,gid}`, `get_peer_creds(&stream)`, `authorise(&creds, namtor_auth_gid)`, `resolve_namtor_auth_gid()`.
`namtor_daemon::nonce`: `NonceService::new()`, `issue() -> [u8;16]`, `validate_reply(issued, echoed)`.
`namtor_daemon::listener`: `LifecycleDispatch` trait (`dispatch(&AuthBody)`, `snapshot()`), `Listener::bind(path, auth_gid, dispatcher)`, `Listener::from_fd(fd, path, auth_gid, dispatcher)` (socket activation), `accept_one()`, `shutdown()`, `path()`.
`namtor_daemon::models`: `ModelFetcher` + `HttpFetch` trait, `ensure_int8(spec)`.
`namtor_daemon::licence`: `LicenceRecord`, `ensure_licence_accepted(model_dir, model_spec)`.
`namtor_daemon::lifecycle`: `LcPhase{Cold,Warming,Verifying}`, `Lifecycle<C:Clock>` + `prepare_for_auth`/`verify_start`/`release`/`tick`/`warm_remaining_ms`/`snapshot`, `Clock` trait, `SystemClock`, `MockClock::new(Instant)` + `advance(Duration)` (cloneable, `Arc<Mutex<Instant>>`).
`namtor_daemon::supervisor`: `WorkerSupervisor` trait (`load_all`, `infer`, `shutdown`), `MockSupervisor::new()`, `ChildSupervisor::spawn(worker_bin, models, mock_infer)` (socketpair + memfd).
`namtor_daemon::daemon`: `Daemon<S:WorkerSupervisor, C:Clock>` + `new(sup, models, warm_timeout, clock, capture_factory)`, `phase()`, `loaded_models()`, `capture_open()`, `prepare_for_auth`/`verify_start`/`release`/`tick`/`snapshot`, `supervisor_mut()`. `CaptureFactory = Box<dyn FnMut() -> Result<Box<dyn CaptureSource>> + Send>`.

## Hardware facts (don't re-probe)

ASUS ROG Flow X16 GV601VI, Arch Linux, kernel 7.1.x, systemd 261, GNOME/Wayland (GDM). Intel i9-13900H (`avx2`+`avx_vnni`, no AVX-512/AMX). TPM 2.0 (`/dev/tpmrm0`, tss2), Secure Boot disabled. Sonix `3277:0018` on USB `3-8`: IR `/dev/video2` (L8 `{00000032-0002-…}`, 640×360@30), IR metadata `/dev/video3`, RGB `/dev/video0`, RGB metadata `/dev/video1`. MSXU unit 4 selectors 6+9 on interface 2; selector 6 mode 2 = firmware-strobed emitter (R3 PASS). R1 (dual-stream) PASS. R2 (AE hunt) resolved Tier 0. **R10 (emitter survives suspend) untested — needs real S3 cycle.** R11 (camera-open cost) ~677 ms upper bound, re-measured in-process in M2.

**User is NOT in `video` or `tss` groups** — desktop camera works via logind seat ACL, which does NOT extend to system daemons. The `namtor` system user needs real group membership (M3).

## What M3 needs from the human (ask, don't fake)

1. **Root** — install systemd units, create `namtor` user + `namtor-auth` group, setuid `namtor-worker-spawn`. Provide commands when ready.
2. **Python onnxruntime binding** — `onnxruntime-cpu` 1.28.0 pacman package installed BUT `python3 -c "import onnxruntime"` fails (likely Python 3.14 incompat). Blocks R4 (INT8 vs FP32) + Task 16. Needs a compatible Python venv or binding fix.
3. **Model selection + licence acceptance** — which stock models (YuNet/MobileFaceNet/AdaFace/MiniFASNet-class) + accepting upstream licences (Task 15 records it; `NAMTOR_ACCEPT_LICENCE=1`).
4. **R5 cohort dataset** — which licence-verified dataset forms the background cohort for score normalisation. Decision recorded in `docs/m3-r4-report.md`.

Everything else in M3 is CI-testable with mocks (no camera, no root, no weights).

## Hard rules (enforced by CI, not review)

- TDD: failing test → minimal impl → green → commit per task.
- SPDX header on every `.rs`. `namtor-hwdb`/`namtor-probe`/`quirks/` Apache-2.0; rest GPL-3.0-only.
- HW-gated tests `#[ignore]` with reason `"requires the reference camera; run with NAMTOR_HW_TEST=1"`. CI never sets it.
- No `unwrap()`/`expect()` in non-test code. No `as any`-equivalent. `unsafe` only for FFI with SAFETY comments.
- No model weights, datasets, or biometric data committed. No biometric data in logs at any level — **asserted by a test**, not review.
- Security invariants asserted by tests: `SO_PEERCRED` on every connection; worker cannot open files/network/unauthorised syscalls; `PAM_RHOST` set → refuse.
- No GPU EP in the auth path (CPU INT8/AVX-VNNI is the design). The sandbox never widens silently.
- Don't expose raw match thresholds in any GUI. Don't make PAD a weighted term (it's a veto). Don't prompt before MSXU probing.

## Decisions already made (don't relitigate)

PAM integration (not direct SDDM/GDM). Rust core + Python ML tooling only. Four processes, three trust levels (inference in `nobody` worker under seccomp+landlock). TPM2-sealed templates, device-bound, non-exportable. Strobe-differential liveness, analytic gates first. Dual-camera RGB+IR fusion with enrollment-fitted extrinsics. CPU-only auth path. GPL-3.0 core / Apache-2.0 hardware DB.

## What NOT to do

- Don't write M4-M10 plans before M3's measurements (R4/R5) land.
- Don't add a GPU EP to the auth path.
- Don't commit anything to `fixtures/` with a real face without recorded consent.
- Don't ship emitter control without watchdog + start-up reconciliation.
- Don't skip suspend/resume (lid-open is the most common interaction).
