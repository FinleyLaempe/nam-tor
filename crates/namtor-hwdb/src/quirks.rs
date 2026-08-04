// SPDX-License-Identifier: Apache-2.0
//! Device quirks TOML schema and loader.
//!
//! One TOML file per device under `quirks/devices/`, keyed by `vid:pid`.
//! The schema mirrors the real TOML exactly (nested `[device]`/`[nodes]`/
//! `[emitter]`/`[emitter.strobe]`/`[meta]` sections) so the file is the
//! single source of truth and the struct is its typed projection.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::HwdbError;

/// Top-level device identifiers.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCfg {
    pub vid_pid: String,
    pub name: String,
    #[serde(default)]
    pub laptop: Option<String>,
    #[serde(default)]
    pub uvc_version: Option<String>,
}

/// USB interface numbers and the IR format GUID. Interface numbers, never
/// `/dev/videoN` — node numbering is allocation-order dependent.
#[derive(Debug, Clone, Deserialize)]
pub struct NodesCfg {
    pub ir_vc_interface: u8,
    pub ir_streaming: u8,
    #[serde(default)]
    pub rgb_vc_interface: Option<u8>,
    #[serde(default)]
    pub rgb_streaming: Option<u8>,
    pub ir_format_guid: String,
    #[serde(default)]
    pub ir_resolution: Option<String>,
    pub metadata_node: bool,
    pub depth: bool,
}

/// Emitter control method. Probing tier follows the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmitterMethod {
    MsxuMode,
    Msxu,
    VendorXu,
    AlwaysOn,
    FirmwareStrobed,
    Unknown,
}

/// Strobe timing parameters measured under the active mode.
#[derive(Debug, Clone, Deserialize)]
pub struct StrobeCfg {
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub duty_cycle: Option<f64>,
    #[serde(default)]
    pub illuminated_mean: Option<f64>,
    #[serde(default)]
    pub clean_mean: Option<f64>,
    #[serde(default)]
    pub persistent_across_streams: Option<bool>,
    #[serde(default)]
    pub persistent_across_suspend: Option<String>,
    #[serde(default)]
    pub settle_ms: u32,
    #[serde(default)]
    pub use_metadata: bool,
}

/// Emitter behaviour with no control applied.
#[derive(Debug, Clone, Deserialize)]
pub struct IdleCfg {
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub mean_luma: Option<f64>,
    #[serde(default)]
    pub alternation: Option<f64>,
}

/// Maximum continuous emitter on-time before the watchdog forces it off.
#[derive(Debug, Clone, Deserialize)]
pub struct LifecycleCfg {
    pub max_on_ms: u32,
}

/// Emitter control descriptor. `payload_on`/`payload_off` are hex strings in
/// the TOML, deserialised to byte vectors.
#[derive(Debug, Clone, Deserialize)]
pub struct EmitterCfg {
    pub method: EmitterMethod,
    #[serde(default)]
    pub guid: Option<String>,
    #[serde(default)]
    pub vc_interface: Option<u8>,
    #[serde(default)]
    pub unit: Option<u8>,
    #[serde(default)]
    pub selector: Option<u8>,
    #[serde(default)]
    pub length: Option<u8>,
    #[serde(deserialize_with = "deserialize_hex_payload")]
    pub payload_on: Vec<u8>,
    #[serde(deserialize_with = "deserialize_hex_payload")]
    pub payload_off: Vec<u8>,
    #[serde(default)]
    pub mode: Option<u8>,
    #[serde(default)]
    pub strobe: Option<StrobeCfg>,
    #[serde(default)]
    pub idle: Option<IdleCfg>,
    #[serde(default)]
    pub lifecycle: Option<LifecycleCfg>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExposureCfg {
    #[serde(default)]
    pub ae_hunts: Option<bool>,
    #[serde(default)]
    pub ae_affects: Option<String>,
    #[serde(default)]
    pub ae_settle_pairs: Option<u32>,
    #[serde(default)]
    pub requires_per_pair_norm: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BandwidthCfg {
    #[serde(default)]
    pub concurrent_ir_rgb: Option<bool>,
    #[serde(default)]
    pub ir_fps_solo: Option<f64>,
    #[serde(default)]
    pub ir_fps_concurrent: Option<f64>,
    #[serde(default)]
    pub rgb_fps_concurrent: Option<f64>,
    #[serde(default)]
    pub pair_ms_solo: Option<u32>,
    #[serde(default)]
    pub pair_ms_concurrent: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LatencyCfg {
    #[serde(default)]
    pub stream_open_ms_upper_bound: Option<u32>,
    #[serde(default)]
    pub per_frame_ms: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccessCfg {
    #[serde(default)]
    pub requires_root: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamCfg {
    #[serde(default)]
    pub msxu_metadata_available: Option<bool>,
    #[serde(default)]
    pub candidate_kernel_patch: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetaCfg {
    pub verified_by: String,
    pub verified_on: String,
    pub kernel: String,
    #[serde(default)]
    pub method_note: Option<String>,
}

/// One quirks entry, mirroring a single device TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct QuirksEntry {
    pub device: DeviceCfg,
    pub nodes: NodesCfg,
    pub emitter: EmitterCfg,
    #[serde(default)]
    pub exposure: Option<ExposureCfg>,
    #[serde(default)]
    pub bandwidth: Option<BandwidthCfg>,
    #[serde(default)]
    pub latency: Option<LatencyCfg>,
    #[serde(default)]
    pub access: Option<AccessCfg>,
    #[serde(default)]
    pub upstream: Option<UpstreamCfg>,
    pub meta: MetaCfg,
}

/// The loaded quirks database, keyed by `device.vid_pid` (e.g. `"3277:0018"`).
#[derive(Debug, Clone, Default)]
pub struct QuirksDb {
    pub entries: HashMap<String, QuirksEntry>,
}

/// Parses a hex payload string (`"01030200..."`) to bytes.
pub fn parse_hex_payload(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hex = std::str::from_utf8(chunk).ok()?;
        out.push(u8::from_str_radix(hex, 16).ok()?);
    }
    Some(out)
}

/// serde deserialiser for hex-encoded payload strings.
fn deserialize_hex_payload<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    parse_hex_payload(&s).ok_or_else(|| serde::de::Error::custom("invalid hex payload"))
}

/// Loads every `*.toml` under `dir` (recursively) into a `QuirksDb`, keyed by
/// `device.vid_pid`. A malformed file surfaces as `HwdbError::Parse`.
pub fn load_quirks_dir(dir: &Path) -> Result<QuirksDb, HwdbError> {
    let mut files = Vec::new();
    collect_toml_files(dir, &mut files)?;
    files.sort();

    let mut entries = HashMap::new();
    for file in files {
        let text = fs::read_to_string(&file).map_err(HwdbError::from)?;
        let entry: QuirksEntry = toml::from_str(&text)
            .map_err(|e| HwdbError::Parse(format!("{}: {e}", file.display())))?;
        entries.insert(entry.device.vid_pid.clone(), entry);
    }
    Ok(QuirksDb { entries })
}

fn collect_toml_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), HwdbError> {
    let it = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(HwdbError::from(e)),
    };
    for entry in it.flatten() {
        let path = entry.path();
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(HwdbError::from(e)),
        };
        if meta.is_dir() {
            collect_toml_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_reference_quirks(dir: &Path) -> PathBuf {
        let src = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../quirks/devices/3277-0018.toml"
        );
        let dst = dir.join("3277-0018.toml");
        fs::copy(src, &dst).unwrap();
        dst
    }

    #[test]
    fn loads_the_reference_entry() {
        let tmp = tempfile::tempdir().unwrap();
        copy_reference_quirks(tmp.path());

        let db = load_quirks_dir(tmp.path()).expect("load failed");
        assert_eq!(db.entries.len(), 1);

        let entry = db.entries.get("3277:0018").expect("keyed by vid:pid");
        assert_eq!(entry.nodes.ir_streaming, 3);
        assert_eq!(entry.emitter.method, EmitterMethod::MsxuMode);
        assert_eq!(entry.emitter.mode, Some(2));
        assert_eq!(
            entry.emitter.payload_on,
            vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0]
        );
        let strobe = entry.emitter.strobe.as_ref().expect("strobe section");
        assert_eq!(strobe.settle_ms, 6);
        assert!(strobe.use_metadata);
        assert_eq!(entry.meta.verified_by, "finley");
    }

    #[test]
    fn malformed_toml_is_a_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("bad.toml"), "this is = = not toml").unwrap();
        let err = load_quirks_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, HwdbError::Parse(_)));
    }

    #[test]
    fn parse_hex_payload_round_trips() {
        assert_eq!(
            parse_hex_payload("010302000000000000"),
            Some(vec![0x01, 0x03, 0x02, 0, 0, 0, 0, 0, 0])
        );
        assert_eq!(parse_hex_payload(""), Some(Vec::new()));
        assert!(parse_hex_payload("abc").is_none(), "odd length is invalid");
    }
}
