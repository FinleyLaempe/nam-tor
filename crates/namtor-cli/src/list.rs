// SPDX-License-Identifier: GPL-3.0-only
//! List enrolled users.

use std::path::Path;
use std::process::ExitCode;

use namtor_store::TemplateStore;

pub fn run_list(templates_dir: &Path, key_path: &Path) -> ExitCode {
    let store = match TemplateStore::open(templates_dir, key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("store open failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    match store.list_uids() {
        Ok(uids) if uids.is_empty() => {
            println!("no enrollments");
            ExitCode::SUCCESS
        }
        Ok(uids) => {
            println!("enrolled users:");
            for uid in uids {
                match store.load(uid) {
                    Ok(record) => {
                        println!(
                            "  uid={} ({}) — {} templates, model={}",
                            record.meta.uid,
                            record.meta.username,
                            record.meta.num_templates,
                            record.meta.model_id,
                        );
                    }
                    Err(e) => {
                        println!("  uid={} — error: {e}", uid);
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("list failed: {e}");
            ExitCode::FAILURE
        }
    }
}
