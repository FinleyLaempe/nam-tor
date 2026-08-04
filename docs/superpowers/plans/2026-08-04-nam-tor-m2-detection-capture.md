# Nam-tor M2 — Detection and Capture Implementation Plan

> Execute task-by-task in TDD order (failing test → minimal impl → green → commit). Each task lists Files, Interfaces (binding contract), test behavior, impl shape, commit message. Write the actual Rust code TDD-style from these specs.

**Goal:** detection (`namtor-hwdb`), probing (`namtor-probe`), production capture (`IrUvc`/`RgbUvc`/`CaptureSession` in `namtor-capture`), camera/emitter runtime manager (`namtor-camera`) covering lifecycle/resume/hotplug/always-on. Advance G1.1–G1.6 (G1.6 HW-in-the-loop).

**Architecture:** 4 new crates. `namtor-hwdb` (Apache-2.0): sysfs descriptor parse, format-GUID IR classification, USB→V4L2 node correlation, quirks TOML loader/matcher. `namtor-probe` (Apache-2.0): tiered emitter discovery CLI, candidate quirks entry emission. Production V4L2 MMAP sources (`IrUvc`/`RgbUvc`/`CaptureSession`) in existing `namtor-capture` (Linux-only modules, HW tests `#[ignore]`). `namtor-camera` (GPL-3.0, NEW — deviation from §15, justified: roadmap places lifecycle/resume/hotplug in M2 pre-daemon): emitter controller, watchdog, resume reconciler, hotplug monitor, multi-camera selection, always-on detection. `namtor-cli` (GPL-3.0): `status` subcommand.

**Tech Stack:** Rust, `nix` (V4L2 ioctls+mmap), `toml` (quirks), `zbus` (logind D-Bus), `udev` (hotplug), `clap` (CLIs). Deps on M1 `namtor-uvc` (Apache-2.0) + M0 `namtor-capture` (GPL-3.0).

## Global Constraints

- Rust edition 2021+. SPDX header on every `.rs`. `namtor-hwdb`/`namtor-probe`/`quirks/` Apache-2.0; `namtor-capture`/`namtor-camera`/`namtor-cli` GPL-3.0-only.
- TDD: failing test → minimal impl → green → commit per task.
- HW-gated tests `#[ignore]` with reason `"requires the reference camera; run with NAMTOR_HW_TEST=1"`. CI never sets it. Non-ignored tests must pass.
- Probe safety (spec §4.3) structurally enforced by M1 `SafeXu` — reuse, never reimplement. No `#[allow]` to suppress clippy. `unsafe` only for V4L2/ioctl FFI.
- Reuse M1 `namtor-uvc` modules (descriptors, probe/SafeXu/XuTransport, discover, mode, emitter/verify_emitter, device/V4l2Device, strobe, metadata) and M0 `namtor-capture` (CaptureSource, Frame, SourceCaps, Spectrum, SessionWriter). Do not reimplement.
- V4L2 MMAP via `nix` directly — no `v4l`/`v4l2` ecosystem crate (spec §4.11.2).
- No model weights/datasets/biometric data in logs.
- Reference HW: Arch Linux, kernel 7.1.x, Sonix 3277:0018 on USB 3-8, IR node /dev/video2, IR metadata /dev/video3, RGB /dev/video0.

### Deviations from spec

1. `CaptureSource` pull-based (`next_frame`) — established M0.
2. New crate `namtor-camera` (not in §15 list). Additive, GPL-3.0. Roadmap puts lifecycle/resume/hotplug in M2.
3. Metadata-node detection via sysfs `name` substring "metadata" (case-insensitive). Pragmatic, documented.
4. `zbus` + `udev` crates added (spec §4.10.3/§4.10.4 require D-Bus+udev; spec didn't name crates).
5. Production capture sources in `namtor-capture` (new Linux-only modules `v4l2`,`ir_uvc`,`rgb_uvc`,`session`) not a new crate — keeps §15 layout. `namtor-capture` gains `nix`+`namtor-uvc` deps.

### Reused M1/M0 API surface (binding)

`namtor_uvc::descriptors`: `parse_descriptors(&[u8]) -> DescriptorSet`, `DescriptorSet{extension_units, formats}`, `ExtensionUnit{interface,unit_id,guid:[u8;16],bm_controls}` + `advertised_selectors()`, `FormatDescriptor{interface,format_index,guid}`, `format_guid`, `is_l8_ir_format`, `guid_leading_dword`.
`namtor_uvc::xu`: `Query`, `CtrlInfo`.
`namtor_uvc::probe`: `XuTransport`, `SafeXu<T>` (new, probe_control, try_write→Restorer, transport_query), `ControlProfile`, `Restorer`(keep), `XuError`.
`namtor_uvc::device`: `V4l2Device::open(&Path)`, `fd()`, impl XuTransport.
`namtor_uvc::discover`: `discover_emitter(&mut SafeXu, &mut impl FrameSampler, &[ExtensionUnit], &DiscoverCfg) -> Vec<Candidate>`, `DiscoverCfg::default()`, `FrameSampler{sample(&mut self,n) -> Result<Vec<Frame>,XuError>}`, `Candidate`.
`namtor_uvc::mode`: `ModeControl{unit,selector,len,current,max,default}` + `mode_candidates()`, `find_mode_controls(&mut SafeXu,&[ExtensionUnit]) -> Vec<ModeControl>`, `measure_alternation(&[Frame]) -> AlternationEvidence{alternation,on_mean,off_mean,alternates}`.
`namtor_uvc::emitter`: `verify_emitter`, `EmitterEvidence`, `EmitterVerifyCfg`, `luma_stats(&Frame) -> LumaStats`.
`namtor_uvc::metadata`: `parse_meta(&[u8]) -> Option<UvcMetaBuf>` (sof, pts).
`namtor_uvc::strobe`: `classify`, `FrameWindow`, `Toggle`, `StrobeSequencer`.
`namtor_capture::frame`: `PixFmt`(Grey8/Yuyv/Mjpeg/Depth16), `EmitterState`(On/Off/Torn/Unknown), `Frame`, `Frame::validate()`.
`namtor_capture::source`: `Spectrum`(Ir/Rgb/Depth), `SourceCaps{spectrum,format,width,height,fps,has_metadata}`, `CaptureError`(NotStarted/Device/Io/Recording), `CaptureSource{caps,start,next_frame,stop}`.
`namtor_capture::recording`: `SessionLabel`, `SessionWriter`.

---

## Area 1 — `namtor-hwdb`

### Task 1: crate skeleton + sysfs USB enumeration
**Files:** Create `crates/namtor-hwdb/Cargo.toml` (Apache-2.0; deps: serde, thiserror; dev: tempfile), `src/lib.rs`, `src/sysfs.rs`. Modify root `Cargo.toml` (add member).
**Produces:** `UsbDevice{sysfs_path:PathBuf, vid:u16, pid:u16, bcd_device:u16, descriptors_path:PathBuf}`; `enumerate_usb(sysfs_root:&Path) -> Result<Vec<UsbDevice>,HwdbError>`. Reads `/sys/bus/usb/devices/*/` requiring `idVendor`+`idProduct` files; hex parse via `u16::from_str_radix(_,16)`; `descriptors_path=dir/descriptors`. Sorted by sysfs path.
**Test:** fake sysfs tree in tempdir: device with idVendor=3277/idProduct=0018/bcdDevice=0001/descriptors=<fixture>, hub without idVendor. Assert one UsbDevice vid 0x3277 pid 0x0018 bcd 0x0001; hub skipped.
**Commit:** `feat(hwdb): sysfs USB device enumeration`

### Task 2: format-GUID classification
**Files:** Create `src/classify.rs`. Modify lib.rs.
**Produces:** `FormatKind{Ir,Depth,Colour,Unknown}`; `ClassifiedFormat{interface,format_index,guid:[u8;16],kind,guid_str:String}`; `classify_formats(&DescriptorSet) -> Vec<ClassifiedFormat>`. L8 (leading dword 0x32) → Ir; D16 (leading dword 0x40) → Depth; else Colour/Unknown.
**Test:** reference fixture via parse_descriptors; classify_formats → one Ir on interface 3 guid_str starts "00000032-0002-"; a Colour on interface 1; no Depth.
**Commit:** `feat(hwdb): format-GUID IR/depth/colour classification`

### Task 3: USB interface → V4L2 node correlation
**Files:** Create `src/nodes.rs`. Modify lib.rs.
**Produces:** `V4l2Node{dev_path:PathBuf, interface:u8, is_metadata:bool, name:String}`; `correlate_nodes(usb:&UsbDevice) -> Result<Vec<V4l2Node>,HwdbError>`. Walk `<usb>/<cfg>:<iface>/video4linux/` entries (each `videoN`); iface number from parent dir name; is_metadata from `/sys/class/video4linux/videoN/name` contains "metadata" (lowercase).
**Test:** fake sysfs: USB dir `3-8/` with iface dirs `3-8:1.0`,`3-8:1.1`,`3-8:1.3`; video0@1.0, video1@1.1 (name "...Metadata..."), video2@1.3, video3@1.3 (name "...Metadata..."). Assert 4 nodes: video0 iface0 not-meta, video1 iface1 meta, video2 iface3 not-meta, video3 iface3 meta. dev_paths /dev/video0..3.
**Commit:** `feat(hwdb): USB interface to V4L2 node correlation`

### Task 4: XU enumeration MSXU first
**Files:** Create `src/xu_index.rs`. Modify lib.rs.
**Produces:** `is_msxu(guid:&[u8;16]) -> bool` (prefix 0f3f95dc-2632-4c4e); `enumerate_xus(set:&DescriptorSet, interface:u8) -> Vec<ExtensionUnit>` filtered by interface, MSXU first then vendor (stable sort, key `!is_msxu as u8`).
**Test:** reference fixture → enumerate_xus(set,2) returns [MSXU unit4 0f3f95dc, vendor unit3 28f03370]; enumerate_xus(set,0) same order; enumerate_xus(set,99) empty.
**Commit:** `feat(hwdb): extension-unit enumeration, MSXU first`

### Task 5: quirks TOML schema + loader
**Files:** Create `src/quirks.rs`. Modify lib.rs, Cargo.toml (add toml dep). Modify `quirks/devices/3277-0018.toml` (add `mode`, `settle_ms`, `use_metadata` fields — see below).
**Produces:** structs mirroring the **actual** TOML schema (nested, not flat):
- `DeviceCfg{vid_pid:String, name:String, laptop:Option<String>, uvc_version:Option<String>}`
- `NodesCfg{ir_vc_interface:u8, ir_streaming:u8, rgb_vc_interface:Option<u8>, rgb_streaming:Option<u8>, ir_format_guid:String, ir_resolution:Option<String>, metadata_node:bool, depth:bool}` — note `ir_streaming`/`rgb_streaming` are the TOML keys (do NOT rename to ir_interface).
- `EmitterMethod` enum `#[serde(rename_all="snake_case")]` with variants `MsxuMode, Msxu, VendorXu, AlwaysOn, FirmwareStrobed, Unknown`
- `StrobeCfg{pattern:Option<String>, duty_cycle:Option<f64>, illuminated_mean:Option<f64>, clean_mean:Option<f64>, persistent_across_streams:Option<bool>, persistent_across_suspend:Option<String>, settle_ms:u32, use_metadata:bool}` — `settle_ms`/`use_metadata` are NEW fields added by this task (see TOML edit below); use `#[serde(default)]` on them.
- `IdleCfg{verdict:Option<String>, mean_luma:Option<f64>, alternation:Option<f64>}`
- `LifecycleCfg{max_on_ms:u32}`
- `EmitterCfg{method:EmitterMethod, guid:Option<String>, vc_interface:Option<u8>, unit:Option<u8>, selector:Option<u8>, length:Option<u8>, payload_on:Vec<u8>, payload_off:Vec<u8>, mode:Option<u8>, strobe:Option<StrobeCfg>, idle:Option<IdleCfg>, lifecycle:Option<LifecycleCfg>}` — `payload_on`/`payload_off` are hex strings in TOML; use `#[serde(deserialize_with="deserialize_hex_payload")]` producing `Vec<u8>`. `mode` is NEW (see TOML edit); `#[serde(default)]`.
- `ExposureCfg`, `BandwidthCfg`, `LatencyCfg`, `AccessCfg`, `UpstreamCfg`, `MetaCfg{verified_by,verified_on,kernel,method_note:Option<String>}` — mirror TOML; all fields `Option` where the real TOML may omit.
- `QuirksEntry{device:DeviceCfg, nodes:NodesCfg, emitter:EmitterCfg, exposure:Option<ExposureCfg>, bandwidth:Option<BandwidthCfg>, latency:Option<LatencyCfg>, access:Option<AccessCfg>, upstream:Option<UpstreamCfg>, meta:MetaCfg}` — nested, mirrors TOML sections exactly.
- `QuirksDb{entries:HashMap<String,QuirksEntry>}` keyed by `device.vid_pid`.
- `load_quirks_dir(dir:&Path) -> Result<QuirksDb,HwdbError>` walks `dir/**/*.toml`, `toml::from_str::<QuirksEntry>`, indexes by `entry.device.vid_pid`.
- `deserialize_hex_payload` serde fn + public `parse_hex_payload(s:&str) -> Option<Vec<u8>>` helper.
- `HwdbError` variants: `Io(#[from] std::io::Error)`, `Parse(String)`, `Sysfs(String)`, `NotFound(String)`.
**TOML edit (required):** add to `quirks/devices/3277-0018.toml`:
  - under `[emitter]`: `mode = 2` (the strobe mode byte, byte 2 of payload_on)
  - under `[emitter.strobe]`: `settle_ms = 6` and `use_metadata = true`
Commit the TOML edit in this task's commit.
**Test:** copy the UPDATED `3277-0018.toml` to a tempdir; `load_quirks_dir` → one entry key `"3277:0018"`, `entry.nodes.ir_streaming == 3`, `entry.emitter.method == MsxuMode`, `entry.emitter.mode == Some(2)`, `entry.emitter.payload_on == vec![0x01,0x03,0x02,0,0,0,0,0,0]`, `entry.emitter.strobe.as_ref().unwrap().settle_ms == 6`, `entry.emitter.strobe.as_ref().unwrap().use_metadata == true`, `entry.meta.verified_by == "finley"`. Malformed TOML → `HwdbError::Parse`.
**Commit:** `feat(hwdb): quirks TOML schema and loader`

### Task 6: quirks matcher
**Files:** Modify `src/quirks.rs`.
**Produces:** `QuirksDb::match_device(&self,vid:u16,pid:u16,bcd:Option<u16>) -> Option<&QuirksEntry>`; lookup order: `{vid:04x}:{pid:04x}:{bcd:04x}` if bcd given, else `{vid:04x}:{pid:04x}`. `parse_vid_pid("3277:0018") -> Option<(u16,u16)>`. Keys are `entry.device.vid_pid` strings.
**Test:** db with "3277:0018" + "1111:2222:0001". match(0x3277,0x0018,None)→3277 entry; match(0x1111,0x2222,Some(0x0001))→bcd entry; match(0x1111,0x2222,Some(0x0002))→None; match(0x9999,0x9999,None)→None. parse_vid_pid cases.
**Commit:** `feat(hwdb): quirks matcher with bcdDevice wildcard`

### Task 7: classification pipeline
**Files:** Create `src/pipeline.rs`. Modify lib.rs.
**Produces:** `ClassSource{Quirks,FormatGuid,Heuristic}`; `Classification{ir_interface:u8, ir_format_guid:String, rgb_interface:Option<u8>, metadata_node:bool, source:ClassSource, xu_units:Vec<ExtensionUnit>, dev_nodes:Vec<V4l2Node>}`; `classify(usb:&UsbDevice, db:&QuirksDb) -> Result<Classification,HwdbError>`. Order: `db.match_device`→Quirks (use `entry.nodes.ir_streaming` as `ir_interface`, `entry.nodes.ir_format_guid`, `entry.nodes.rgb_streaming` as `rgb_interface`, `entry.nodes.metadata_node`); else parse_descriptors+classify_formats find Ir→FormatGuid; else heuristic lone greyscale non-colour interface→Heuristic (always reported). Always enumerate_xus + correlate_nodes on the IR interface.
**Test:** (a) 3277 quirks in db → source Quirks, ir_interface 3 (from `nodes.ir_streaming`), xu_units MSXU first. (b) empty db, 3277 descriptors → source FormatGuid, ir_interface 3, ir_format_guid starts "00000032-". (c) construct a `DescriptorSet` directly with a YUYV-format interface 0 + a GREY/L8-absent interface 1, no quirks → source Heuristic, picks interface 1 (the non-colour greyscale). Prefer direct `DescriptorSet` construction over hand-crafting a binary blob.
**Commit:** `feat(hwdb): end-to-end classification pipeline quirks→GUID→heuristic`

---

## Area 2 — `namtor-probe`

### Task 8: skeleton + `describe` subcommand
**Files:** Create `crates/namtor-probe/Cargo.toml` (Apache-2.0; deps: namtor-hwdb, namtor-uvc, namtor-capture, clap), `src/lib.rs`, `src/bin/namtor-probe.rs`. Modify root Cargo.toml.
**Produces:** `run_describe(usb:&UsbDevice, db:&QuirksDb) -> String` (human report: VID:PID, IR interface, IR format GUID, source, XUs MSXU-first with advertised_selectors, /dev/videoN nodes). Binary `namtor-probe describe [--sysfs-root PATH]`.
**Test:** run_describe on fake 3277 UsbDevice + loaded quirks → contains "3277:0018", "IR interface: 3", "MSXU", "source: quirks"; XU lines list selectors 6,9 for MSXU on interface 2.
**Commit:** `feat(probe): describe subcommand for device classification`

### Task 9: tiered probing (MSXU silent → vendor consent)
**Files:** Create `src/probe.rs`. Modify lib.rs, binary (add `probe` subcommand).
**Produces:** `trait ConsentGate{fn request_vendor_probe(&self,vid:u16,pid:u16)->bool}`; `ProbeResult{vid_pid,method:EmitterMethod,unit:u8,selector:u8,payload_on:Vec<u8>,payload_off:Vec<u8>,mode:Option<u8>,evidence:String}`; generic `run_probe<T:XuTransport,S:FrameSampler>(xu:&mut SafeXu<T>, sampler:&mut S, ir_xus:&[ExtensionUnit], consent:&dyn ConsentGate) -> ProbeResult`. MSXU loop: find_mode_controls, for each ModeControl try mode_candidates(), set via try_write (Restorer RAII), sampler.sample(), measure_alternation → if alternates record method=MsxuMode+mode+payload_on, restore default. Vendor loop (only if no MSXU result AND consent.request_vendor_probe() true): discover_emitter over vendor XUs → method=VendorXu. No consent prompt for MSXU (G1.4). Live wrapper `run_probe_live` builds V4l2Device+IrUvc (HW-gated #[ignore] test).
**Test:** (a) fake transport (M1 FakeCamera pattern) + FrameSampler fake returning alternating frames when mode2 set + AutoConsent → method=MsxuMode, mode=Some(2); consent call counter==0 (assert never called). (b) no-MSXU-mode fake + AutoDeny → method=Unknown. (c) same + AutoAllow → method=VendorXu.
**Commit:** `feat(probe): tiered emitter probing, MSXU silent, vendor consented`

### Task 10: candidate quirks entry + gh pr create
**Files:** Create `src/emit.rs`. Modify lib.rs, binary (add `emit` subcommand).
**Produces:** `render_quirks_entry(result:&ProbeResult, name:&str) -> String` (TOML, redacted verified_by="namtor-probe"); `gh_pr_create_invocation(entry_path:&Path) -> String`; `render_redacted_report(result:&ProbeResult) -> String` (no biometric data, no "frame"/"image" words).
**Test:** render_quirks_entry on ProbeResult{vid_pid:"3277:0018",method:MsxuMode,unit:4,selector:6,payload_on:[0x01,0x03,0x02,0,0,0,0,0,0],mode:Some(2),..} → contains `[devices."3277:0018"]`, `method = "msxu_mode"`, `selector = 6`; round-trips via toml::from_str::<QuirksEntry>. gh_pr_create_invocation contains "gh pr create" + "3277:0018". render_redacted_report excludes "frame"/"image".
**Commit:** `feat(probe): candidate quirks entry emission and gh pr create invocation`

---

## Area 3 — Production capture in `namtor-capture`

### Task 11: V4L2 MMAP streaming primitive
**Files:** Create `crates/namtor-capture/src/v4l2.rs`. Modify Cargo.toml (add nix ioctl+mman+fs, namtor-uvc path), lib.rs.
**Produces:** `V4l2Format{width,height,pixelformat:u32,bytesperline,sizeimage}`; `V4l2Stream` owning File on /dev/videoN + Vec<MmapBuffer{ptr,len}> + started flag; `open(path)`, `negotiate_format(w,h,fmt)`, `request_buffers(count)`, `mmap_buffers()`, `streamon()`, `dequeue() -> (usize, Duration)`, `requeue(idx)`, `streamoff()`, `read_buffer(idx) -> &[u8]`. unsafe confined to ioctl+mmap with SAFETY comments. V4L2 ioctl codes via nix _IOWR macros. Drop: munmap+streamoff+close.
**Test:** (a) unit test V4l2Format sizeimage sanity (no device). (b) #[ignore] HW: open /dev/video2, negotiate 640×360 GREY (V4L2_PIX_FMT_GREY=0x59455247), 4 buffers, mmap, streamon, dequeue 2, assert sizeimage==230_400, streamoff.
**Commit:** `feat(capture): V4L2 MMAP streaming primitive`

### Task 12: `IrUvc` capture source
**Files:** Create `src/ir_uvc.rs`. Modify lib.rs.
**Produces:** `IrUvc` impl `CaptureSource`. `open(node:&Path, caps:SourceCaps)` opens V4l2Stream, negotiates format from caps. start→streamon; next_frame→dequeue, read_buffer, build Frame format=Grey8, sof from parse_meta (if has_metadata open second V4l2Stream on metadata node, dequeue+parse), emitter via parity (msxu_mode: even=On/odd=Off) or strobe classify (vendor); stop→streamoff. A `MockStreamer` trait for unit testing (fake frames → assert caps + emitter tagging).
**Test:** unit test with MockStreamer feeding alternating-brightness frames → IrUvc.next_frame returns Frame with emitter On/Off alternating by parity, caps().spectrum==Ir, validate() ok. #[ignore] HW: open /dev/video2 + /dev/video3, start, dequeue 4 frames, assert sof present, emitter alternates.
**Commit:** `feat(capture): IrUvc capture source with metadata pairing`

### Task 13: `RgbUvc` capture source
**Files:** Create `src/rgb_uvc.rs`. Modify lib.rs.
**Produces:** `RgbUvc` impl CaptureSource over V4l2Stream for RGB node (YUYV or MJPEG). open/negotiate/start/next_frame/stop. emitter=Unknown always (RGB has no emitter). sof from metadata if available.
**Test:** MockStreamer feeding YUYV frames → caps().spectrum==Rgb, validate ok, emitter==Unknown. #[ignore] HW: open /dev/video0, dequeue 2 frames.
**Commit:** `feat(capture): RgbUvc capture source`

### Task 14: `CaptureSession` + R11 in-process open-cost
**Files:** Create `src/session.rs`. Modify lib.rs.
**Produces:** `CaptureSession{ir:IrUvc, rgb:Option<RgbUvc>}`; `open(usb:&UsbDevice, db:&QuirksDb, hw_root:&Path) -> Result<Self,CaptureError>` (classify, open IR node + optional RGB node per quirks). Drop: stop streams (RAII; emitter-off is namtor-camera's job, not here). `measure_open_cost(node:&Path) -> Duration` — opens V4l2Stream, negotiates, streamon, dequeues 1 frame, streamoff, returns elapsed (R11 in-process re-measurement).
**Test:** CaptureSession with MockStreamer-backed IrUvc → Drop calls stop (assert a stop flag). measure_open_cost on a mock returns >0 Duration. #[ignore] HW: measure_open_cost("/dev/video2") prints elapsed ms (updates quirks [latency] via probe, not here).
**Commit:** `feat(capture): CaptureSession RAII and in-process camera-open cost (R11)`

---

## Area 4 — `namtor-camera` (emitter lifecycle)

### Task 15: crate skeleton + EmitterController + start-up reconciliation
**Files:** Create `crates/namtor-camera/Cargo.toml` (GPL-3.0; deps: namtor-uvc, namtor-hwdb, namtor-capture, thiserror), `src/lib.rs`, `src/emitter_ctrl.rs`. Modify root Cargo.toml.
**Produces:** `EmitterController<T:XuTransport>` wrapping SafeXu + QuirksEntry.emitter config; `apply_on()` writes payload_on (or sets mode for MsxuMode) via try_write, keeps Restorer internally; `off()` writes payload_off (or mode=default); `verify()` GET_CUR round-trip; `current_mode()`. Drop: force off (RAII). Start-up reconciliation: `force_off_before_anything()` called first in apply path. A `Clock` trait (now()) for watchdog testability (Task 16).
**Test:** FakeDevice transport (M1 pattern) + quirks MsxuMode payload_on=[010302…]; apply_on then drop → off restored (GET_CUR reads default). force_off_before_anything writes payload_off before any apply. verify() returns current bytes.
**Commit:** `feat(camera): EmitterController with RAII off and start-up reconciliation`

### Task 16: watchdog + ExecStopPost teardown
**Files:** Create `src/watchdog.rs`. Modify lib.rs. Create `src/bin/namtor-camera-emitter-off.rs` (helper invoked by systemd ExecStopPost).
**Produces:** `Watchdog{max_on_ms:u32 (default 2000 from quirks), clock:&dyn Clock, armed:bool}`; `arm()` starts timer; `check_expired() -> bool` true if elapsed>max_on_ms; on expiry caller forces emitter off. `Clock` trait with `now() -> Instant`; `SystemClock` impl; `MockClock` for tests. Binary `namtor-camera-emitter-off <node>` opens V4l2Device + SafeXu + writes payload_off (best-effort, logs errors).
**Test:** MockClock advanced past max_on_ms → check_expired() true; before → false. Watchdog arm+check+disarm cycle. Binary is thin; test the lib force_off function it calls (FakeDevice).
**Commit:** `feat(camera): emitter watchdog and ExecStopPost teardown helper`

---

## Area 5 — resume (spec §4.10.3)

### Task 17: logind PrepareForSleep subscription
**Files:** Create `src/resume.rs`. Modify lib.rs, Cargo.toml (add zbus).
**Produces:** `trait SleepSignal{fn on_resume(&self)}`; `ResumeMonitor` subscribing to org.freedesktop.login1.Manager PrepareForSleep (false=resume) via zbus; `run_loop(signal:&dyn SleepSignal)` blocking; `ResumeState{Reconciling,Ready}` state machine. The zbus wiring behind a function; the state machine testable via a `SleepEvent` enum injected in tests.
**Test:** state machine: initial Ready; on SleepEvent::Suspend → Reconciling; on SleepEvent::Resume → trigger reconcile then Ready. No real D-Bus in test (inject events).
**Commit:** `feat(camera): logind PrepareForSleep subscription and resume state machine`

### Task 18: resume reconciliation (device re-detect + emitter re-apply, requests queued)
**Files:** Modify `src/resume.rs`.
**Produces:** `Reconciler` on resume: re-runs hwdb classify, reopens V4l2Device, re-applies emitter mode (EmitterController.apply_on), verifies. While Reconciling, auth requests queued (`PendingState::Reconciling`) not failed; on Ready, queued requests serviced. `queue_request(req) -> PendingState`; `drain_ready() -> Vec<Request>`.
**Test:** inject Suspend→Resume; assert a request queued during Reconciling is held (returns Reconciling), then drained after Ready. FakeDevice transport; assert apply_on called once on resume.
**Commit:** `feat(camera): resume reconciliation with request queuing`

---

## Area 6 — hotplug (spec §4.10.4) + namtor-cli

### Task 19: udev add/remove detection
**Files:** Create `src/hotplug.rs`. Modify lib.rs, Cargo.toml (add udev crate).
**Produces:** `DeviceEvent{Add(node:V4l2Node), Remove(node)}; `trait DeviceEventSource{fn next_event(&mut self) -> Option<DeviceEvent>}`; `UdevMonitor` impl via udev crate (Socket, receive_device). Event-processing logic testable via `FakeEventSource`.
**Test:** FakeEventSource feeds Add(video2) then Remove(video2) → a `HotplugState` transitions: has_device true after Add, false after Remove.
**Commit:** `feat(camera): udev hotplug add/remove detection`

### Task 20: deterministic multi-camera selection
**Files:** Create `src/selection.rs`. Modify lib.rs.
**Produces:** `select_camera(candidates:&[V4l2Node], configured:Option<&str>, enrolled:Option<&str>) -> Option<SelectedDevice>`; order: configured → enrolled → internal (non-removable per sysfs `removable` file == "fixed") → first classified IR. `SelectedDevice{node:V4l2Node, reason:SelectReason}`.
**Test:** 3 candidates (internal video2, external video4, external video6); no configured/enrolled → picks internal video2 (removable=="fixed"). configured="video6" → picks video6. enrolled="video4", no configured → picks video4. empty → None.
**Commit:** `feat(camera): deterministic multi-camera selection order`

### Task 21: `namtor-cli` skeleton + `status`
**Files:** Create `crates/namtor-cli/Cargo.toml` (GPL-3.0; deps: namtor-camera, namtor-hwdb, clap), `src/lib.rs`, `src/bin/namtor-cli.rs`. Modify root Cargo.toml.
**Produces:** `run_status(camera:&CameraManager) -> String` reporting selected device (dev path, vid:pid, emitter method), stream state (open/closed, who requested), protection level (Full/Reduced for always_on). Binary `namtor-cli status`.
**Test:** run_status on a CameraManager with selected /dev/video2, stream closed, protection Full → contains "/dev/video2", "stream: closed", "protection: full". With always_on → "protection: reduced".
**Commit:** `feat(cli): namtor-cli skeleton and status subcommand`

---

## Area 7 — always-on (spec §4.10.5)

### Task 22: always-on detection + reduced-protection surfacing
**Files:** Create `src/always_on.rs`. Modify lib.rs.
**Produces:** `detect_always_on(idle_baseline_frames:&[Frame], with_emitter_off_frames:&[Frame]) -> bool` — true if frames are bright (mean>threshold) with emitter control off AND no alternation (measure_alternation.alternates==false). Reuse luma_stats + measure_alternation. `ProtectionLevel{Full,Reduced}`; when method=always_on → Reduced, surfaced in namtor-cli status (Task 21) and recorded in quirks emitter.method=AlwaysOn.
**Test:** (a) bright steady frames with emitter-off → detect_always_on true. (b) alternating frames → false. (c) dark frames (mean<5) → false (no emitter, not always-on). ProtectionLevel Reduced when always_on.
**Commit:** `feat(camera): always-on emitter detection and reduced-protection surfacing`

---

## M2 completion criteria

- [ ] `cargo test --all` passes (non-ignored; HW-gated `#[ignore]` not run)
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `./scripts/check-spdx.sh` passes
- [ ] `namtor-probe describe` runs against the reference fixture + sysfs and reports the IR interface, MSXU XUs, /dev/videoN
- [ ] `namtor-probe probe` (HW-gated, `#[ignore]`) reaches method=MsxuMode mode=2 on /dev/video2
- [ ] `namtor-cli status` reports the selected device and protection level
- [ ] Quirks entry `3277-0018.toml` loads via `load_quirks_dir` and round-trips through `render_quirks_entry`
- [ ] CaptureSession Drop stops streams (RAII) on MockStreamer
- [ ] Watchdog fires after max_on_ms on MockClock
- [ ] Resume state machine queues requests during Reconciling
- [ ] Hotplug selection order: configured → enrolled → internal → first IR

### Gate coverage

| Gate | Status in M2 |
|---|---|
| G1.1 zero-config on known device | PASS if `classify` returns source=Quirks for 3277 and CaptureSession opens (CI, no camera) |
| G1.2 automatic probe on unknown device | Code complete; HW-gated `#[ignore]` (needs on-device run) |
| G1.3 specific message per failure mode | Partial — failure messages flow through namtor-probe/namtor-cli; full §8.3 table is M5 |
| G1.4 zero interaction on MSXU | PASS if `run_probe` MSXU tier never calls ConsentGate (CI, fake transport) |
| G1.5 emitter off after teardown | Code complete; HW-gated luminance check `#[ignore]` |
| G1.6 first unlock after S3 | HW-in-the-loop only — `#[ignore]` test stub, needs 20 on-device S3 cycles |

**What M2 unblocks:** M3 (daemon hosts `namtor-camera`; worker consumes `IrUvc`/`RgbUvc` frames); R9 (always-on) and R10 (resume) codified; R11 re-measured in-process.
