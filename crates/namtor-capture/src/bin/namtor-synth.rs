// SPDX-License-Identifier: GPL-3.0-only
//! Generates a synthetic capture session for testing and CI.
//!
//! Synthetic sessions are labelled as such and are never included in
//! published error rates.

use namtor_capture::recording::SessionLabel;
use namtor_capture::synth::{synth_session, SynthGeometry, SynthOpts};
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ! {
    eprintln!("usage: namtor-synth --out <dir> [--pairs N] [--seed N] [--geometry face|flat]");
    std::process::exit(2);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut out: Option<PathBuf> = None;
    let mut pairs: u32 = 5;
    let mut seed: u64 = 0;
    let mut geometry = SynthGeometry::Face;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out = Some(PathBuf::from(args.get(i).unwrap_or_else(|| usage())));
            }
            "--pairs" => {
                i += 1;
                pairs = args
                    .get(i)
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
            }
            "--seed" => {
                i += 1;
                seed = args
                    .get(i)
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage());
            }
            "--geometry" => {
                i += 1;
                geometry = match args.get(i).map(String::as_str) {
                    Some("face") => SynthGeometry::Face,
                    Some("flat") => SynthGeometry::Flat,
                    _ => usage(),
                };
            }
            _ => usage(),
        }
        i += 1;
    }

    let Some(out) = out else { usage() };

    let opts = SynthOpts {
        pairs,
        seed,
        geometry,
        label: SessionLabel::Synthetic,
    };

    match synth_session(&out, &opts) {
        Ok(()) => {
            println!("wrote {} frames to {}", pairs * 2, out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
