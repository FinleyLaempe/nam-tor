# Nam-tor M3 — Daemon and Sandboxed Worker Implementation Plan

> Execute task-by-task in TDD order (failing test → minimal impl → green → commit). Each task lists Files, Interfaces (binding contract), test behavior, impl shape, commit message. Write the actual Rust code TDD-style from these specs.

**Goal:** the four-process privilege-separated core (spec §3): `namtor-daemon` (user `namtor`, groups `video`+`tss`), `namtor-worker` (user `nobody`, seccomp + landlock, no fs/net/IPC but the pipe), the `SO_PEERCRED` socket ACL with nonce-bound replies (spec §3.2), the `InferenceBackend` trait + `OnnxCpu` with INT8/AVX-VNNI (spec §5.6, §5.1), stock downloaded weights with upstream licence acceptance, and the warm-models / cold-camera lifecycle (spec §5.5). Advance **G5.1** (structurally: the wire crate carries no ML) and **G5.2** (worker sandbox escape tests); resolve **R4** (INT8 vs FP32 via the M0 eval harness) and **R5** (cohort sourcing decision recorded).

**Architecture:** 2 new runtime crates + 1 new shared wire crate. `namtor-proto` (GPL-3.0, NEW): two independent modules — `pam` (daemon↔PAM socket: `SO_PEERCRED`-authenticated, nonce-bound request/reply) and `worker` (daemon↔worker pipe: frame→vector + Load/Loaded control). `namtor-worker` (GPL-3.0, NEW): pipe transport, `InferenceBackend` trait, `OnnxCpu` (`ort` crate), self-applied seccomp + landlock sandbox, main loop. `namtor-daemon` (GPL-3.0, NEW): socket listener + ACL + nonce service, model fetcher, warm/cold lifecycle state machine, worker supervisor. `namtor-cli` (existing): `status` extended to report stream open state and who requested it. systemd units + sysusers/tmpfiles under `packaging/`.

**Tech Stack:** Rust, `ort` (ONNX Runtime bindings, CPU EP, INT8 quantized models), `serde`+`serde_json` (wire), `nix` (`SO_PEERCRED`, socket, setuid), `libseccomp` crate (seccomp filter), `landlock` crate (fs/net restrictions), `clap` (daemon/worker CLIs), `zbus` (logind, reuse M2), `reqwest` (model download, feature-gated rustls). Deps on M2 `namtor-camera` + `namtor-capture` + `namtor-hwdb`, M0 `namtor-eval`.

## Global Constraints

- Rust edition 2021+. SPDX header on every `.rs`; CI asserts via `scripts/check-spdx.sh`. `namtor-proto`, `namtor-worker`, `namtor-daemon` are **GPL-3.0-only**.
- TDD: failing test → minimal impl → green → commit per task.
- HW-gated / on-device tests `#[ignore]` with reason `"requires the reference camera/machine; run with NAMTOR_HW_TEST=1"`. CI never sets it. Non-ignored tests must pass with **no camera and no model weights present**.
- No `unwrap()`/`expect()` in non-test code. No `unsafe` suppression, no `#[allow]` to silence clippy. `unsafe` only for FFI (nix/libseccomp land) with SAFETY comments.
- No model weights, datasets, or biometric data committed. No biometric data in logs at any level (spec §8.6).
- Security invariants asserted by tests, not review (roadmap): `SO_PEERCRED` on every connection; worker cannot open files/network/unauthorised syscalls.
- Reference HW: Arch Linux, kernel 7.1.x, systemd 261, Intel i9-13900H (AVX2 + AVX-VNNI), Sonix `3277:0018`, IR `/dev/video2`, metadata `/dev/video3`.
- Reuse M2/M1/M0 crates; do not reimplement capture, emitter, resume, hotplug, selection, eval metrics.

### Deviations from spec

1. **New `namtor-proto` crate** (not in §3.3 components table). Justified: the daemon↔PAM socket and the daemon↔worker pipe are two distinct seams (spec §3, §3.2) but both need shared, version-pinned wire types; a dedicated crate prevents the PAM module (M5) from depending on daemon internals and lets CI round-trip both protocols without hardware. Two modules keep the seams independent; PAM will depend only on `namtor-proto` (types-only, no ML deps — advances G5.1).
2. **Worker self-applies its sandbox.** The worker binary applies seccomp + landlock early in `main()`, rather than the daemon applying them pre-exec. Rationale: the daemon is not root (spec §3.1) and cannot setuid to `nobody`; landlock restrictions would block library loading if applied before exec. A small setuid helper (`namtor-worker-spawn`) performs the drop to `nobody` and is installed by packaging; the worker's self-sandbox is what the escape tests assert (the G5.2 invariant). The setuid drop itself is an `#[ignore]` test requiring root.
3. **Device-free testing trait is `Streamer`** (M2 actual), not `MockStreamer` as the M3 brief named it. A `MockStreamer` impl exists for tests. Reused verbatim.
4. **Toy ONNX fixture committed for backend mechanics.** `fixtures/onnx/identity.onnx` (a tiny identity model, < 1 KB) is a test artefact, not a face-recognition weight; explicitly labelled and excluded from the "no weights committed" rule by the same logic as M0's synthetic fixtures. No face model weights are committed.
5. **INT8 quantization is a committed recipe, not a committed weight.** `ml/recipes/quantize_int8.py` (onnxruntime static quantization with a calibration set from `fixtures/`) generates `int8.onnx` on the user's machine from the fetched FP32 stock model. The fetcher invokes it when Python + onnxruntime are available; otherwise it falls back to FP32 and reports degraded mode in `namtor-cli status`.

### Reused M2/M1/M0 API surface (binding)

`namtor_capture::source`: `Spectrum{Ir,Rgb,Depth}`, `SourceCaps{spectrum,format,width,height,fps,has_metadata}`, `CaptureError{NotStarted,Device,Io,Recording}`, `CaptureSource{caps,start,next_frame,stop}` (pull-based; `next_frame` returns `Ok(None)` at end-of-stream).
`namtor_capture::frame`: `PixFmt{Grey8,Yuyv,Mjpeg,Depth16}`, `EmitterState{On,Off,Torn,Unknown}`, `Frame`, `Frame::validate()`.
`namtor_capture::session`: `CaptureSession::new(ir: Box<dyn CaptureSource>, rgb: Option<Box<dyn CaptureSource>>)`, RAII `Drop`, `measure_open_cost(node:&Path) -> Result<Duration,CaptureError>`, `measure_open_cost_with(f: impl FnOnce()) -> Duration`.
`namtor_capture::ir_uvc` / `rgb_uvc`: `IrUvc<S:Streamer,P:EmitterPolicy>`, `RgbUvc<S:Streamer>`; `V4l2IrStream::open`, `V4l2RgbStream::open` (Linux-only, HW tests `#[ignore]`).
`namtor_camera::emitter_ctrl`: `EmitterController<'a,T:XuTransport>::new(&'a mut SafeXu<T>, &'a EmitterCfg)`, `force_off_before_anything`, `apply_on`, `off`, `verify`, `method`, `is_armed`; `EmitterCtrlError`.
`namtor_camera::watchdog`: `trait Clock`, `SystemClock`, `MockClock::new(Instant)` + `advance(Duration)`, `Watchdog<'a,C:Clock>::new(max_on_ms:u32, &'a C)`, `from_quirks(&EmitterCfg,&'a C)`, `arm`, `disarm`, `is_armed`, `check_expired`; `force_off_lib<T:XuTransport>(&mut SafeXu<T>,&EmitterCfg)`, `force_off_on_device(&Path,&EmitterCfg)`.
`namtor_camera::resume`: `SleepEvent`, `ResumeState`, `ResumeMachine::new()` + `state()` + `handle(event,&dyn SleepSignal)` + `mark_ready()`, `Request`, `PendingState`, `Reconciler<F:FnMut()>::new(on_reconcile)` + `state()` + `pending_count()` + `handle(event)` + `queue_request(req)` + `drain_ready()`.
`namtor_camera::hotplug`: `DeviceEvent`, `trait DeviceEventSource`, `UdevMonitor::new()`, `HotplugState::new()` + `has_device()` + `apply(event)` + `drain(&mut S)`, `FakeEventSource::new(Vec<DeviceEvent>)`.
`namtor_camera::selection`: `SelectReason`, `SelectedDevice`, `select_camera(...)`.
`namtor_camera::always_on`: `detect_always_on(idle_baseline:&[Frame], with_emitter_off:&[Frame]) -> bool`, `ProtectionLevel`.
`namtor_cli::status`: `ProtectionLevel`, `StreamState`, `CameraManager{..}` + `new(..)`, `run_status(&CameraManager) -> String`. M3 Task 20 extends `CameraManager` with stream-requester info.
`namtor_hwdb`: `QuirksDb`, `QuirksEntry`, `EmitterCfg`, `EmitterMethod`, `classify`, `UsbDevice`, `V4l2Node`, `enumerate_usb`.
`namtor_eval::score`: `ScoreRecord`, `LatencyBreakdown`, `write_jsonl`, `read_jsonl`. M3 Task 25 reuses the JSONL score format for R4 FP32-vs-INT8 comparison.

---

## Area 1 — `namtor-proto` (shared wire protocol)

### Task 1: crate skeleton + worker wire messages + framing
**Files:** Create `crates/namtor-proto/Cargo.toml` (GPL-3.0; deps: serde, serde_json, thiserror, namtor-capture), `src/lib.rs`, `src/worker.rs`, `src/framing.rs`. Modify root `Cargo.toml` (add member).
**Produces:** `pub mod worker` with messages: `ModelSpec{id:String, quant:Quant, sha256:[u8;32]}`, `Quant{Fp32,Int8}`, `WorkerReq{Load(ModelSpec), Infer(FrameRef), Shutdown}`, `FrameRef{offset:u64, len:u32, emitter:EmitterState, spectrum:Spectrum, pix_fmt:PixFmt, w:u32, h:u32, ts_us:u64}` (metadata only — pixel bytes travel via shared `memfd`, not serialised), `WorkerRsp{Loaded(Loaded), Vector(Vec<f32>), Error(String)}`, `Loaded{model_id:String, quant:Quant, input_shape:Vec<u64>, output_shape:Vec<u64>}`. `src/framing.rs`: `encode(msg:&impl Serialize, w:&mut impl Write) -> Result<(),ProtoError>` (4-byte BE len + JSON payload), `decode<R:Read>(r:&mut R) -> Result<serde_json::Value,ProtoError>` (read len, read payload, parse). `ProtoError{Io,Json(String),Truncated}`.
**Test:** round-trip each `WorkerReq`/`WorkerRsp` variant through `encode`→`decode` → equal. Truncated stream (len=100, only 4 bytes follow) → `ProtoError::Truncated`. Bad JSON payload → `ProtoError::Json`.
**Commit:** `feat(proto): worker wire messages and length-delimited framing`

### Task 2: pam wire messages + nonce envelope
**Files:** Create `src/pam.rs`. Modify lib.rs.
**Produces:** `pub mod pam` with: `AuthBody{nonce:[u8;16], target_uid:u32, phase:AuthPhase, rhost:Option<String>, deadline_us:u64}`; `AuthPhase{Prepare,Verify,Release}` (spec §5.5.2 three-phase lifecycle — the wire carries the phase so the daemon knows whether to `prepare_for_auth`, `verify_start`, or `release`); `AuthRequest` is an **enum** `AuthRequest{ Auth(AuthBody), Status }` (Status is the read-only status query used by `namtor-cli status`, Task 20 — no camera_lock, no serialisation); `AuthReply{nonce:[u8;16], verdict:Verdict, score:f32, reason:Option<String>}`; `StatusReply{stream:StreamState, requester:Option<Requester>, warm_remaining_ms:Option<u64>}`; `Verdict{Match,NoMatch,Unavailable,RateLimited,Refused}`; `HelloMsg{server_version:u16, nonce:[u8;16]}` (daemon issues nonce on connect; client echoes it in `AuthBody.nonce`). `rhost` set → daemon returns `Refused` (spec §8.2). Serialised-request semantics (spec §3.2) live in the daemon listener (Task 13), not proto; proto carries the nonce + phase only. `Requester{uid:u32, label:String}` is shared between `StatusReply` and the lifecycle (Task 17).
**Test:** round-trip `AuthRequest{Auth(AuthBody{..phase:Prepare..})}`, `AuthRequest{Status}`, `AuthReply{..Verdict::Match score 0.83..}`, `StatusReply`, `HelloMsg` via framing → equal after round-trip. Nonce is exactly 16 bytes. `AuthPhase` round-trips all three variants.
**Commit:** `feat(proto): pam wire messages with nonce-bound replies`

---

## Area 2 — `namtor-worker` (sandboxed inference host)

### Task 3: crate skeleton + pipe transport
**Files:** Create `crates/namtor-worker/Cargo.toml` (GPL-3.0; deps: namtor-proto, serde, serde_json, thiserror, ort, nix; dev: namtor-capture), `src/lib.rs`, `src/pipe.rs`, `src/bin/namtor-worker.rs`. Modify root Cargo.toml.
**Produces:** `pub mod pipe` with `PipeTransport{reader, writer}`; `PipeTransport::from_fds(read_fd: RawFd, write_fd: RawFd) -> Result<Self,WorkerError>` (nix `FromRawFd`), `send(&mut self, rsp:&WorkerRsp) -> Result<(),WorkerError>` (framing encode), `recv(&mut self) -> Result<WorkerReq,WorkerError>` (framing decode + typed deserialise). `WorkerError{Pipe(#[from]ProtoError), Io, BadFd, Sandbox(String), Infer(String)}`. Binary `namtor-worker` takes `--read-fd N --write-fd M` and runs the main loop (Task 9).
**Test:** in-process: `nix::sys::socket::socketpair`, two `PipeTransport` on opposite ends, send `WorkerReq::Load(ModelSpec{id:"test".into(),quant:Fp32,sha256:[0;32]})` one way → equal on recv; send `WorkerRsp::Vector(vec![0.1,0.2])` back → equal. EOF on recv when peer closed → explicit EOF variant (document which).
**Commit:** `feat(worker): pipe transport over inherited fds`

### Task 4: InferenceBackend trait + MockBackend
**Files:** Create `src/infer.rs`. Modify lib.rs.
**Produces:** `pub trait InferenceBackend { fn load(&mut self, spec:&ModelSpec) -> Result<Loaded,WorkerError>; fn run(&self, loaded:&Loaded, frame:&[u8], meta:&FrameRef) -> Result<Vec<f32>,WorkerError>; }`; `pub struct MockBackend{ out_dim: usize }` returning deterministic pseudo-embeddings (frame-len-hash seeded LCG into `out_dim` floats) so tests are reproducible; `pub fn default_backend() -> Box<dyn InferenceBackend>` returns `OnnxCpu` by default, `MockBackend` when crate feature `mock-infer` is on (default off).
**Test:** `MockBackend::load(ModelSpec{id:"m".into(),quant:Fp32,..})` → `Loaded{model_id:"m",..}`. `run` on same frame bytes twice → identical vectors (determinism). Different frame bytes → different vectors. `default_backend()` under `--features mock-infer` returns MockBackend (cfg-checked).
**Commit:** `feat(worker): InferenceBackend trait and MockBackend`

### Task 5: OnnxCpu backend (ort crate, INT8/AVX-VNNI)
**Files:** Modify `src/infer.rs`. Add `fixtures/onnx/identity.onnx` (tiny identity model, < 1 KB; generate via committed `scripts/gen-identity-onnx.py` run once; labelled "test artefact, not a model weight"). Modify Cargo.toml (`ort` dep; decide static link default, document).
**Produces:** `pub struct OnnxCpu{ env: ort::environment::Environment, threads: usize }`; `OnnxCpu::new(threads:usize) -> Result<Self,WorkerError>`; impl `InferenceBackend`: `load` resolves onnx path by `spec.id` + `spec.quant` (fp32/int8 file), creates `ort::session::Session` with `OptimizationLevel::Level3` + `intra_op_num_threads`; AVX-VNNI is automatic on the CPU EP (no explicit flag; if the `ort` API exposes an AVX-VNNI config, set it, else document EP auto-detect); returns `Loaded` with input/output shapes. `run` builds an `ort::value::Tensor` from `frame` (cast u8→f32 per model input dtype), runs, extracts `f32` output vector.
**Note (verify before implementing):** confirm `ort` 2.x API for `SessionBuilder`, `Tensor::from_array`, CPU EP options via a `librarian` query; record the resolved API shape in a comment at the top of `infer.rs`.
**Test (CI, no weights):** load `fixtures/onnx/identity.onnx` (input `[1,N]` f32, identity output), run with `[1.0,2.0,3.0]` → output `[1.0,2.0,3.0]` within 1e-5. `Loaded` shapes match fixture. `Quant::Fp32` vs `Quant::Int8` file-selection branch tested with a mock path resolver (real INT8 quantization is Task 16's recipe).
**Commit:** `feat(worker): OnnxCpu backend with INT8/AVX-VNNI via ort`

### Task 6: seccomp filter + escape tests
**Files:** Create `src/sandbox/mod.rs`, `src/sandbox/seccomp.rs`. Modify Cargo.toml (`libseccomp` dep), lib.rs.
**Produces:** `pub fn apply_seccomp() -> Result<(),WorkerError>` using `libseccomp` crate, deny-by-default. Allowlist: `read,write,close,fcntl,fstat,lseek,mmap,mprotect,munmap,brk,rt_sigaction,rt_sigprocmask,rt_sigreturn,ioctl,recvfrom,sendto,shutdown,poll,nanosleep,clock_gettime,getpid,exit,exit_group`. Deny (ERRNO EPERM): `open,openat,creat,socket,connect,bind,listen,accept,socketpair` (socketpair used pre-sandbox only; worker inherits connected fds), `execve,execveat,fork,clone,clone3`. `WorkerError::Sandbox(String)`.
**Test:** spawn child `namtor-worker --seccomp-self-test <kind>` that applies seccomp then attempts the action, reports errno. Kinds: `open` (`open("/etc/passwd")` → EPERM), `socket` (→ EPERM), `write` (to stdout → succeeds). Assert each. Idempotency: `apply_seccomp` twice → no error.
**Commit:** `feat(worker): seccomp syscall filter with escape tests`

### Task 7: landlock restrictions + escape tests
**Files:** Create `src/sandbox/landlock.rs`. Modify Cargo.toml (`landlock` dep), lib.rs.
**Produces:** `pub fn apply_landlock() -> Result<(),WorkerError>` using `landlock` crate (Linux 5.13+; reference kernel 7.1.x supports it). FS: no read/write/execute on any path (worker has no on-disk model; libs already mapped before apply). Network bind/connect via landlock `Network` rules (kernel 6.7+) if the crate exposes them; if the running kernel lacks net landlock, the seccomp `socket` block (Task 6) is the primary net barrier — document the fallback. `WorkerError::Sandbox(String)`.
**Test:** spawn child `namtor-worker --landlock-self-test <kind>` applying landlock then attempting: `open` (`open("/etc/passwd",O_RDONLY)` → EACCES/EPERM), `mkdir` (`/tmp/namtor-escape-test` → EACCES). Positive control: `read` on an inherited pre-opened fd succeeds post-apply.
**Commit:** `feat(worker): landlock fs/net restrictions with escape tests`

### Task 8: combined sandbox self-application + escape suite (G5.2)
**Files:** Modify `src/sandbox/mod.rs` (combine), create `tests/escape.rs` (integration test crate). Modify worker binary (`--apply-sandbox` flag).
**Produces:** `pub fn apply_sandbox() -> Result<(),WorkerError>` = `apply_landlock()` then `apply_seccomp()` (order documented: landlock first, seccomp stricter and survives). Integration test `tests/escape.rs` runs `namtor-worker --apply-sandbox --self-test <kind>` for `open_file`, `open_socket`, `connect`, `exec`, `fork`; each applies the sandbox then attempts the action, reports errno. Asserts each denied (G5.2 invariant: worker cannot open files, network sockets, or unauthorised syscalls — spec §5.3, §11.4).
**Test:** the escape suite is the test. Assert all 5 kinds → denied. Positive control: `write` to inherited pipe fd succeeds post-sandbox.
**Commit:** `feat(worker): combined seccomp+landlock sandbox with escape suite (G5.2)`

### Task 9: worker main loop
**Files:** Modify `src/lib.rs` (`run_main`), `src/bin/namtor-worker.rs` (arg parse → `run_main`).
**Produces:** `pub fn run_main(read_fd: RawFd, write_fd: RawFd, backend: Box<dyn InferenceBackend>, apply_sbx: bool, frame_memfd: Option<RawFd>, frame_region_len: u64) -> Result<(),WorkerError>`: construct `PipeTransport`, mmap `frame_memfd` read-only BEFORE `apply_sandbox()` (frame bytes shared via `memfd_create`; daemon writes frames, `FrameRef.offset/len` index into the region), optionally `apply_sandbox()`, loop: `recv()` → match `WorkerReq`: `Load(spec)` → `backend.load(spec)` → `send(WorkerRsp::Loaded(..))`; `Infer(frame_ref)` → read frame bytes at `offset..offset+len` from the memfd region → `backend.run(loaded, frame_bytes, frame_ref)` → `send(WorkerRsp::Vector(..))`; `Shutdown` → break. On `WorkerError` → `send(WorkerRsp::Error(..))` then continue (exit on pipe-fatal). Document the memfd decision in `src/lib.rs` header.
**Test:** in-process: socketpair, `memfd_create` with a canned frame, spawn `run_main` in a thread with `MockBackend` (no sandbox), drive from the other end: `Load(m)` → `Loaded`; `Infer(frame_ref)` → `Vector` of `out_dim` floats; `Shutdown` → thread exits cleanly. Verify `FrameRef` indexing matches memfd contents.
**Commit:** `feat(worker): main loop over pipe with Load/Infer/Shutdown`

<!-- CHUNK-2-END -->

---

## Area 3 — Socket ACL (`namtor-daemon`)

### Task 10: crate skeleton + SO_PEERCRED extraction
**Files:** Create `crates/namtor-daemon/Cargo.toml` (GPL-3.0; deps: namtor-proto, namtor-camera, namtor-capture, namtor-hwdb, namtor-worker, nix, serde, serde_json, thiserror, clap; dev: tempfile), `src/lib.rs`, `src/acl.rs`, `src/bin/namtor-daemon.rs`. Modify root Cargo.toml.
**Produces:** `pub fn get_peer_creds(stream:&impl AsFd) -> Result<PeerCreds,DaemonError>` via `nix::sys::socket::getsockopt` `SO_PEERCRED` → `PeerCreds{pid:i32, uid:u32, gid:u32}`. `DaemonError{Socket, Acl, Proto, Worker, State, Io}`.
**Test:** socketpair, `get_peer_creds` on one end → uid/gid equal the test process's own (`nix::unistd::getuid`/`getgid`). Bad fd → `DaemonError::Socket`.
**Commit:** `feat(daemon): SO_PEERCRED peer credential extraction`

### Task 11: ACL decision (uid 0 OR namtor-auth group)
**Files:** Modify `src/acl.rs`.
**Produces:** `pub fn authorise(creds:&PeerCreds, namtor_auth_gid: u32) -> bool` (uid==0 || gid==namtor_auth_gid). `pub fn resolve_namtor_auth_gid() -> Result<u32,DaemonError>` via parsing the group file (env override `NAMTOR_GROUP_FILE` for tests; production reads `/etc/group`). Missing group → `DaemonError::Acl("namtor-auth group not found")`. GDM runs as `gdm`, added to `namtor-auth` by packaging (Task 23).
**Test:** `authorise(uid:0,_,1000)` → true; `authorise(uid:1000,gid:1000,1000)` → true; `authorise(uid:1000,gid:999,1000)` → false. `resolve_namtor_auth_gid` against a temp group file → resolves; missing → error.
**Commit:** `feat(daemon): uid-0 or namtor-auth group ACL decision`

### Task 12: nonce service (issue + bind reply + reject replay)
**Files:** Create `src/nonce.rs`. Modify lib.rs, Cargo.toml (`rand` dep).
**Produces:** `pub struct NonceService{ seen: Mutex<HashSet<[u8;16]>>, rng: Mutex<ChaCha20Rng> }`; `issue(&self) -> [u8;16]`, `validate_reply(&self, req_nonce:[u8;16], reply_nonce:[u8;16]) -> Result<(),DaemonError>` — `reply_nonce` must equal `req_nonce` AND not previously seen; mark seen on success. Daemon issues nonce on connect (`HelloMsg`), client echoes it in `AuthRequest.nonce`, daemon returns `AuthReply.nonce` = same. Anti-replay: a captured success reply cannot be replayed (spec §3.2).
**Test:** issue two nonces → distinct. `validate_reply(req,req)` → Ok; `validate_reply(req,other)` → Err. Re-`validate_reply` with same nonce → Err (already seen). Concurrent issue (10 threads × 1000 calls) → no collisions.
**Commit:** `feat(daemon): nonce service with anti-replay`

### Task 13: socket listener (mode 0660, serialised requests, exclusive camera)
**Files:** Create `src/listener.rs`, `src/bin/namtor-daemon.rs`. Modify lib.rs.
**Produces:** `pub struct Listener{ path: PathBuf, listener: UnixListener, auth_gid: u32, nonce: NonceService, camera_lock: Mutex<()>, dispatcher: Arc<dyn LifecycleDispatch> }`. `trait LifecycleDispatch { fn dispatch(&self, body:&AuthBody) -> Result<AuthReply,DaemonError>; fn snapshot(&self) -> StatusReply; }` (Task 17 implements it; Task 13 takes the trait so it tests ACL/nonce/framing with a stub dispatcher). `Listener::bind(path:&Path, auth_gid:u32, dispatcher: Arc<dyn LifecycleDispatch>) -> Result<Self,DaemonError>` (set mode 0660, owner root:namtor-auth — tests skip chown if not root; assert mode via `nix::sys::stat::fstatat`); `accept_one(&self) -> Result<(),DaemonError>`: accept → `get_peer_creds` → `authorise` (deny → close, no reply, fail-closed) → issue nonce → send `HelloMsg` → read `AuthRequest` → match: `Status` → `dispatcher.snapshot()` → write `StatusReply` (no camera_lock); `Auth(body)` → reject if `body.rhost.is_some()` (spec §8.2 → `Verdict::Refused`) → `validate_reply(body.nonce,..)` → acquire `camera_lock` (serialised; spec §3.2) → `dispatcher.dispatch(&body)` → write returned `AuthReply`. Binary `namtor-daemon` wires `Listener::bind` + accept loop.
**Test (ACL + nonce + framing only; full lifecycle round-trip is Task 26):** tempdir socket + stub `LifecycleDispatch` returning `Verdict::Unavailable`. (a) same-uid connect, send `AuthRequest::Status` → `StatusReply` received with matching nonce. (b) ACL deny: inject fake gid via test seam `Listener::with_acl_override` → connection closed, no `HelloMsg`. (c) replay: second `AuthRequest::Auth` with a seen nonce → connection close + log. (d) two concurrent `Auth` connections → second waits on `camera_lock` until first completes (assert ordering via timestamps). (e) `AuthBody{rhost:Some("x"),..}` → `AuthReply` verdict `Refused`. Task 26 asserts the full prepare→verify→infer→release round-trip against the real `Lifecycle` impl.
**Commit:** `feat(daemon): socket listener with ACL, nonce, serialised requests`

---

## Area 4 — Model fetcher (`namtor-daemon`)

### Task 14: model fetcher (download + sha256 verify)
**Files:** Create `src/models.rs`, `crates/namtor-daemon/models.toml` (manifest of stock model IDs + upstream URLs + SHA256 + licence URL — NOT weights). Modify Cargo.toml (`reqwest` rustls-tls, `sha2`), lib.rs.
**Produces:** `pub struct ModelFetcher{ root: PathBuf, client: reqwest::blocking::Client }`; `ModelFetcher::new(root:&Path) -> Self`; `fetch(&self, spec:&ModelSpec, source_url:&str, expected_sha256:[u8;32]) -> Result<PathBuf,DaemonError>`: GET url, stream to `<root>/<id>/<quant>.onnx.tmp`, verify sha256, rename to `<quant>.onnx`, write `<root>/<id>/<quant>.sha256`. Idempotent: existing file + matching sha256 → return path without download.
**Test:** `wiremock`/`httptest` server serves a 200-byte blob; fetch → file at expected path, sha256 matches; second fetch → no re-download (count requests). Tampered blob (wrong sha256) → `DaemonError::State("sha256 mismatch")`, `.tmp` removed.
**Commit:** `feat(daemon): model fetcher with sha256 verification`

### Task 15: licence acceptance record + idempotency
**Files:** Modify `src/models.rs`. Create `src/licence.rs`.
**Produces:** `pub struct LicenceRecord{ model_id:String, licence_url:String, licence_sha256:[u8;32], accepted_at_us:u64, accepted_by:String }`; `ensure_licence_accepted(&self, spec:&ModelSpec, licence_url:&str, licence_sha256:[u8;32], accepted_by:&str) -> Result<(),DaemonError>`: writes `<root>/<id>/licence.accepted` (JSON). Exists → Ok. Not exists + env `NAMTOR_ACCEPT_LICENCE=1` → write record. Not exists + env unset → `DaemonError::State("licence not accepted; run namtor-cli models accept <id>")` (fail-closed). Loader (Task 21) calls this before `OnnxCpu::load`.
**Test:** no record + env unset → Err. env set → record written, second call Ok. Tampered licence sha256 → Err. Record parses back to `LicenceRecord`.
**Commit:** `feat(daemon): upstream licence acceptance record`

### Task 16: INT8 quantization recipe + fetcher integration
**Files:** Create `ml/recipes/quantize_int8.py` (onnxruntime `quantize_static` with calibration data from `fixtures/`), `ml/recipes/README.md`. Modify `src/models.rs`.
**Produces:** `quantize_int8.py --fp32 <path> --out <path> --calib <dir>` runs ORT static INT8 quantization. `ModelFetcher::ensure_int8(&self, spec:&ModelSpec) -> Result<PathBuf,DaemonError>`: if `<root>/<id>/int8.onnx` exists + sha256 → return; else if `python3 -c "import onnxruntime"` available → run `quantize_int8.py` on fetched fp32 → write int8.onnx + sha256; else → `Err(State("int8 unavailable; python/onnxruntime missing"))` — caller falls back to fp32 and records degraded mode.
**Test:** (CI) `ensure_int8` with python missing (env `NAMTOR_PYTHON=/nonexistent`) → Err. (dev) `#[ignore]` test: real fp32 fixture → quantize → int8.onnx exists and loads via `OnnxCpu` (used by Task 25 R4).
**Commit:** `feat(daemon): INT8 quantization recipe and fetcher integration`

<!-- CHUNK-3-END -->

---

## Area 5 — Warm models / cold camera lifecycle

### Task 17: daemon lifecycle state machine (mocked capture)
**Files:** Create `src/lifecycle.rs`. Modify lib.rs.
**Produces:** `pub enum LcState{ Cold, Warming{ since: Instant, requester: Requester }, Verifying{ since: Instant, requester: Requester }, Releasing{ since: Instant } }`; `Requester{ uid:u32, label:String }` (from `PeerCreds`). `pub struct Lifecycle<C: Clock>{ state: LcState, clock: &C, warm_timeout: Duration, capture: Option<Box<dyn CaptureSource>>, emitter_off: bool }` (emitter + real capture wired in Task 19; here use a `MockCaptureSource` seam). Methods: `prepare_for_auth(&mut self, req:Requester) -> Result<(),DaemonError>` (Cold→Warming: start mock source, mark emitter-on; Warming→no-op; Verifying→Err("already verifying")), `verify_start(&mut self, req:Requester) -> Result<(),DaemonError>` (Warming→Verifying), `release(&mut self) -> Result<(),DaemonError>` (→Releasing→Cold: stop source, emitter off), `tick(&mut self)`: Warming past `warm_timeout` → auto `release()`. Spec §5.5.2.
**Test (mocked):** `MockClock`, `MockCaptureSource` (impls `CaptureSource`, returns canned frames, records start/stop calls). Cold → prepare → Warming (source started, emitter-on recorded). Warming → verify_start → Verifying. Verifying → release → Releasing → Cold (source stopped, emitter off). `tick` with `MockClock` advanced past `warm_timeout` while Warming → auto release to Cold. Verifying → prepare → Err.
**Commit:** `feat(daemon): warm/cold lifecycle state machine`

### Task 18: warm-stream 30s timeout
**Files:** Modify `src/lifecycle.rs`.
**Produces:** `warm_timeout: Duration` default 30 s (spec §5.5.2). `tick` is called by the daemon's main loop on each iteration (Task 22 wires a periodic timer). Abandoned warm stream (no `VerifyStart`) closes after 30 s → camera LED dark, privacy property restored. `namtor-cli status` (Task 20) reports remaining warm time.
**Test:** `MockClock` advanced to 29s → still Warming; to 31s → `tick` triggers release → Cold. Custom `warm_timeout` (5s) honoured.
**Commit:** `feat(daemon): warm-stream 30s idle timeout`

### Task 19: boot — pre-load models into warm worker, open NO camera
**Files:** Modify `src/lifecycle.rs`, `src/lib.rs` (`Daemon::new` wires a `WorkerSupervisor` (Task 21) + `IrUvc`/`EmitterController` factory). Add a `WorkerSupervisor` **trait** seam in `src/supervisor.rs` (Task 21 implements it) so Task 19 tests with a mock.
**Produces:** `Daemon::new` takes a `Box<dyn WorkerSupervisor>` + a capture/emitter factory closure. On start: `supervisor.load_all(models)` → worker warm; `LcState::Cold`; no `IrUvc`/`EmitterController` constructed (factory not invoked). First `prepare_for_auth` invokes the factory lazily. This task defines the `WorkerSupervisor` trait + mock; Task 21 provides the real impl and re-runs the boot test against the real worker (moving the `#[ignore]` HW variant there).
**Test (CI, mock supervisor):** `Daemon::new` with a `MockSupervisor` (records `load_all` calls) + a capture factory that panics if invoked → boot reaches `Cold`, `load_all` called once, factory NEVER invoked (assert via the panic-seam). (HW) `#[ignore]` boot test deferred to Task 21.
**Commit:** `feat(daemon): warm worker, cold camera at boot`

### Task 20: extend `namtor-cli status` (stream open + requester)
**Files:** Modify `crates/namtor-cli/src/status.rs` (`CameraManager`, `run_status`), `crates/namtor-cli/src/bin/namtor-cli.rs`. Reuse the PAM socket with the `AuthRequest::Status` variant (defined in Task 2 — no D-Bus dependency in M3).
**Produces:** `CameraManager` gains `stream: StreamState` (existing) + `stream_requester: Option<Requester>` + `warm_remaining_ms: Option<u64>`. `run_status` output adds lines: `stream: open (requested by uid 1000 "gdm", warm 12.3s remaining)` or `stream: closed`. Daemon answers `Status` requests without acquiring `camera_lock` (read-only, no serialisation against auth).
**Test:** `run_status` with `StreamState::Open`, requester uid 1000 label "gdm", warm 12300 ms → output contains `stream: open`, `uid 1000`, `gdm`, `12.3s`. Closed → `stream: closed`. Daemon `Status` handler returns current `Lifecycle` snapshot without blocking (test against an in-process daemon with mocked lifecycle).
**Commit:** `feat(cli): status reports stream open state and requester`

---

## Area 6 — daemon↔worker pipe integration

### Task 21: daemon worker supervisor
**Files:** Create `crates/namtor-daemon/src/supervisor.rs`. Modify lib.rs, Cargo.toml (`memfd` via `nix`, `libc`).
**Produces:** `pub struct WorkerSupervisor{ child: Child, pipe: PipeTransport, frame_memfd: Memfd, frame_region_len: u64, loaded: HashMap<String, Loaded> }`; `spawn(models_dir:&Path, backend_threads:usize) -> Result<Self,DaemonError>`: `memfd_create("namtor-frames")` sized for N frames, `socketpair`, `Command::new("namtor-worker").arg("--read-fd").arg(..).arg("--write-fd").arg(..).arg("--frame-memfd").arg(..).arg("--frame-len").arg(..).env(..).spawn()` (worker inherits fds), construct `PipeTransport` on the daemon end, `Load` each model spec → collect `Loaded`. `infer(&mut self, model_id:&str, frame:&Frame) -> Result<Vec<f32>,DaemonError>`: write frame bytes into the memfd region at the next slot, send `WorkerReq::Infer(FrameRef{offset,len,..})`, recv `WorkerRsp::Vector`. `shutdown(&mut self)`: send `Shutdown`, wait child. Uses `OnnxCpu` worker (real) or `MockBackend` worker (feature `mock-infer`).
**Test (CI, mock backend):** spawn worker binary built with `--features mock-infer`, `Load` → `Loaded`; `infer` on a canned frame → `Vector` of expected dim; `shutdown` → child exits 0. Frame bytes round-trip through the memfd correctly (worker reads what daemon wrote).
**Commit:** `feat(daemon): worker supervisor with memfd frame channel`

<!-- CHUNK-4-END -->

---

## Area 7 — systemd + packaging

### Task 22: `namtor-daemon.service` + hardening
**Files:** Create `packaging/systemd/namtor-daemon.service`, `packaging/systemd/namtor-daemon.socket` (socket activation for the PAM socket). Modify `crates/namtor-daemon/src/bin/namtor-daemon.rs` (socket-activation: accept the listener fd from systemd via `LISTEN_FDS`).
**Produces:** `namtor-daemon.service`: `User=namtor`, `Group=namtor`, `SupplementaryGroups=video tss`, `ExecStart=/usr/lib/namtor/namtor-daemon`, `ExecStopPost=/usr/lib/namtor/namtor-camera-emitter-off` (reuse M2 binary — belt-and-braces emitter-off on daemon stop), `NoNewPrivileges=true`, `ProtectSystem=strict`, `ProtectHome=true`, `PrivateTmp=true`, `ReadWritePaths=/var/lib/namtor`, `CapabilityBoundingSet=` (empty — daemon is not root, no caps), `RestrictAddressFamilies=AF_UNIX`, `SystemCallFilter=@system-service`, `RestrictNamespaces=true`, `LockPersonality=true`. `namtor-daemon.socket`: `ListenStream=/run/namtor/auth.sock`, `SocketMode=0660`, `SocketUser=root`, `SocketGroup=namtor-auth`, `Service=namtor-daemon.service`. Socket activation means systemd creates the socket with the right mode/owner; the daemon receives the fd.
**Test:** (CI) unit test that `namtor-daemon` accepts a pre-bound listener fd passed via env (`LISTEN_FDS=1` + the fd) instead of `bind`ing itself — simulates socket activation. Assert the daemon serves on the inherited fd. (HW) `#[ignore]`: install the units, `systemctl start`, `systemctl status` shows active, `namtor-cli status` connects.
**Commit:** `feat(daemon): systemd service with hardening and socket activation`

### Task 23: sysusers + tmpfiles + setuid helper
**Files:** Create `packaging/sysusers.d/namtor.conf` (`u namtor - "Nam-tor daemon" /var/lib/namtor /usr/sbin/nologin`), `packaging/tmpfiles.d/namtor.conf` (`d /var/lib/namtor 0750 namtor namtor -`, `d /run/namtor 0750 root namtor-auth -`), `packaging/sysusers.d/namtor-auth.conf` (`g namtor-auth -`, add `gdm` to `namtor-auth` if present). Create `crates/namtor-worker/src/bin/namtor-worker-spawn.rs` (setuid helper: parses `--worker <path> --read-fd N --write-fd M --frame-memfd F`, `setuid(nobody)`, `setgid(nogroup)`, `exec` the worker; root-installed setuid binary). Modify `packaging/README.md` with install instructions.
**Produces:** sysusers create the `namtor` user (groups `video`,`tss` via supplementary config) and `namtor-auth` group; tmpfiles create `/var/lib/namtor` (model store, owner `namtor`) and `/run/namtor` (socket dir, owner `root:namtor-auth`). `namtor-worker-spawn` is the only setuid binary; it drops to `nobody` and execs the worker, which self-applies sandbox. `namtor-daemon.service` `ExecStart` invokes `namtor-worker-spawn` (or the daemon does via `Command`) to start the worker as `nobody`.
**Test:** (CI) `namtor-worker-spawn --dry-run` parses args and prints the planned `setuid`/`exec` without actually running (no root in CI). (HW) `#[ignore]`: as root, install setuid, run spawn, assert worker runs as `nobody` (`getuid` via `--self-test`), sandbox applied.
**Commit:** `feat(packaging): sysusers, tmpfiles, and setuid worker spawn helper`

---

## Area 8 — R4 resolution + completion

### Task 24: G5.1 structural assertion — proto carries no ML
**Files:** Create `crates/namtor-proto/tests/no_ml_deps.rs`. Modify `.github/workflows/ci.yml` (add a step running `cargo tree -p namtor-proto` and asserting no `ort`/`opencv`/`image`/`ndarray` in the output).
**Produces:** Integration test that runs `cargo tree -p namtor-proto --no-default-features` and asserts the output contains none of: `ort`, `onnxruntime`, `opencv`, `image`, `ndarray`, `tch`. This structurally guarantees the future PAM module (M5) can depend on `namtor-proto` without pulling ML code (spec §3.4: "No ML dependencies, no ONNX, no image decoding libraries" — advances G5.1).
**Test:** the test itself. Run `cargo tree -p namtor-proto` and assert absent deps. If a future change adds a heavy dep, this test fails the CI gate.
**Commit:** `test(proto): assert no ML deps in namtor-proto (G5.1)`

### Task 25: R4 resolution — INT8 vs FP32 via M0 eval harness
**Files:** Create `ml/recipes/r4_int8_vs_fp32.py` (drives `namtor-eval` score path on a labelled score set produced by `OnnxCpu` in both `Fp32` and `Int8` modes), `docs/m3-r4-report.md` (template: FAR/FRR/DET curves + per-stage latency for both quant modes, delta table). `#[ignore]` Rust driver `crates/namtor-daemon/tests/r4.rs` that builds both backends, runs them over `fixtures/` score data, writes JSONL via `namtor_eval::score::write_jsonl`, and the Python script produces the report.
**Produces:** R4 (INT8 embedding degradation) resolved with measured evidence: run the M0 eval harness on FP32 and INT8 `OnnxCpu` backends over the same comparison set, compute FAR/FRR at the operating threshold and per-stage latency, record the delta in `docs/m3-r4-report.md`. R5 (normalisation cohort licensing) resolved by recording the cohort-source decision in the same report (which dataset's labelled embeddings form the background cohort, licence verified per `ml/DATASETS.md`). G5.2 escape suite from Task 8 is the G5.2 evidence.
**Test:** (dev-machine, `#[ignore]`) run `cargo test --package namtor-daemon --test r4 -- --ignored` → writes `docs/m3-r4-report.md` with non-empty delta table. CI cannot run this (no weights, no real model); CI runs Task 24 + the escape suite (Task 8) + the mock end-to-end (Task 26).
**Commit:** `docs(m3): R4 INT8-vs-FP32 eval report and R5 cohort decision`

### Task 26: M3 end-to-end CI test + completion gates
**Files:** Create `crates/namtor-daemon/tests/e2e.rs`. Modify `crates/namtor-daemon/src/bin/namtor-daemon.rs` (test seam for in-process run).
**Produces:** End-to-end CI test (no camera, no real weights): in-process daemon with `MockCaptureSource` + `MockBackend` worker. (1) `Listener::bind` on a tempdir socket with real `Lifecycle` (Task 17) + `MockSupervisor` (Task 19 seam, real `WorkerSupervisor` in Task 21). (2) connect, `SO_PEERCRED` passes (same uid), receive `HelloMsg`. (3) send `AuthRequest::Auth(AuthBody{nonce, target_uid, phase:Verify, rhost:None, ..})`. (4) daemon enters `prepare_for_auth` (mock capture starts), `verify_start`, runs `infer` over canned frames → `MockBackend` vectors → daemon decision → `release`. (5) receive `AuthReply{nonce, verdict, score, ..}` with matching nonce. Assert: nonce bound, verdict in `{Match,NoMatch}`, camera_lock held during processing, mock capture stopped after release. Negative paths: `rhost` set → `Refused`; unauthorised uid → close; replayed nonce → close; `phase:Prepare` without Verify → stream stays Warming until 30s timeout (Task 18) → Cold.
**§8.6 no-biometric-in-logs assertion (MAJOR, spec §8.6 + roadmap "asserted by tests, not review"):** the e2e test installs an in-process `tracing` test subscriber capturing all log records emitted by daemon + worker during the full auth cycle, then asserts no record's message or fields contain frame bytes, pixel data, `Vec<f32>` embeddings, or template contents — only uid, verdict, scalar score, latency, reason codes. This is the M3 security-invariant test.
**Test:** the e2e test (including the log-scan assertion) is the test. Plus the M3 completion gate check: `cargo test --all` (non-ignored) green, `cargo clippy --all-targets -- -D warnings` clean, `cargo fmt --all -- --check` clean, `./scripts/check-spdx.sh` passes, `cargo tree -p namtor-proto` clean (Task 24). Document gate coverage in the commit message.
**Commit:** `test(daemon): M3 end-to-end CI harness and completion gates`

---

## M3 completion criteria

- All 26 tasks committed; working tree clean.
- `cargo test --all` (non-ignored) green with **no camera and no model weights** present.
- `cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --all -- --check` clean; `./scripts/check-spdx.sh` passes.
- Worker sandbox escape suite (Task 8) denies `open_file`, `open_socket`, `connect`, `exec`, `fork` — **G5.2 advanced**.
- `namtor-proto` carries no ML deps (Task 24) — **G5.1 advanced** structurally.
- `SO_PEERCRED` verified on every connection (Task 13) + nonce anti-replay (Task 12) — security invariants asserted by tests.
- No biometric data in logs (spec §8.6) — asserted by the Task 26 e2e log-scan test.
- Warm worker / cold camera at boot (Task 19): `namtor-cli status` shows `stream: closed` immediately after daemon start, worker Loaded.
- Warm-stream 30s timeout (Task 18) closes an abandoned warm stream.
- `docs/m3-r4-report.md` records R4 (INT8 vs FP32 delta) and R5 (cohort decision) — dev-machine `#[ignore]` run.
- HW-gated `#[ignore]` tests (real camera boot, setuid spawn, R4) documented and runnable with `NAMTOR_HW_TEST=1`.

## Gate coverage

| Gate | M3 status | Evidence |
|---|---|---|
| G5.1 (no ML in PAM) | **Advanced (structural)** | Task 24: `namtor-proto` has no ML deps; PAM (M5) depends only on proto. |
| G5.2 (worker sandbox) | **Advanced** | Task 8: escape suite denies all 5 kinds; spec §5.3, §11.4. |
| R4 (INT8 degradation) | **Resolved** | Task 25: `docs/m3-r4-report.md` with measured FAR/FRR/latency delta. |
| R5 (cohort licensing) | **Resolved (decision)** | Task 25: cohort source + licence recorded in the R4 report. |

M3 unblocks M4 (enrollment + template storage — daemon hosts the lifecycle; worker hosts `OnnxCpu` for embedding extraction) and M5 (PAM — socket protocol + nonce + ACL defined in `namtor-proto`/`namtor-daemon`).
