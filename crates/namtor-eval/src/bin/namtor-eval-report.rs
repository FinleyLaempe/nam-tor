// SPDX-License-Identifier: GPL-3.0-only
//! Turn a recorded error-rate session into rates with an N and a 95% bound.
//!
//! Reads the JSONL `packaging/hil/error-rates.sh` writes and prints the report
//! G3.2 and G4.2 ask for — per attack instrument, never averaged.

use std::process::ExitCode;

use namtor_eval::session::{score_session, summarise, Attempt, Session};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = None;
    let mut conditions = "unspecified".to_string();
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--in" => {
                input = args.get(i + 1).cloned();
                i += 2;
            }
            "--conditions" => {
                if let Some(v) = args.get(i + 1) {
                    conditions = v.clone();
                }
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: namtor-eval-report --in <attempts.jsonl> \
                     [--conditions <text>] [--json]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown option: {other}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(path) = input else {
        eprintln!("--in <attempts.jsonl> is required");
        return ExitCode::from(2);
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut attempts = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Attempt>(line) {
            Ok(a) => attempts.push(a),
            Err(e) => {
                // Refuse the whole run rather than silently scoring a subset:
                // a rate computed from an unknown fraction of the attempts is
                // worse than no rate.
                eprintln!("{path}:{}: {e}", n + 1);
                return ExitCode::FAILURE;
            }
        }
    }
    if attempts.is_empty() {
        eprintln!("{path}: no attempts");
        return ExitCode::FAILURE;
    }

    let report = score_session(&Session {
        conditions,
        attempts,
    });
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("serialise: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{}", summarise(&report));
    }
    ExitCode::SUCCESS
}
