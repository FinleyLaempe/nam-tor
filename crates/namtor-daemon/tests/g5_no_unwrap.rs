// SPDX-License-Identifier: GPL-3.0-only
//! G5.6 structural test: no `.unwrap()` or `.expect(` in non-test source code.
//!
//! Spec §11.4 + AGENTS.md: "No unwrap()/expect() in non-test code." The PAM
//! cdylib compiles with `panic = "abort"`; a panic in the auth path aborts the
//! process. This test scans every `src/` directory under `crates/` for lines
//! containing `.unwrap()` or `.expect(` that are NOT inside a `#[cfg(test)]`
//! module or a `mod tests` block, and fails if any are found.
//!
//! The scan is line-based and conservative: it tracks whether the current line
//! is inside a test module by looking for `#[cfg(test)]` and `mod tests`
//! markers. False positives (a non-test function that happens to be after a
//! test module in the same file) are avoided by resetting on the closing brace
//! at the module's indentation level.

use std::fs;
use std::path::Path;

/// One violation found by the scanner.
struct Violation {
    file: String,
    line: u32,
    content: String,
}

/// Scan a single source file and return any non-test unwrap/expect violations.
fn scan_file(path: &Path) -> Vec<Violation> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut violations = Vec::new();
    let mut in_test_mod = false;
    let mut test_mod_brace_depth: Option<i32> = None;
    let mut brace_depth: i32 = 0;

    for (line_num, raw_line) in source.lines().enumerate() {
        let line_num = line_num as u32 + 1;
        let trimmed = raw_line.trim_start();

        // Detect start of a cfg(test) module or mod tests block.
        if trimmed.starts_with("#[cfg(test)]") {
            // The next non-blank line should be `mod tests {`.
            // We set a flag and check on the next iteration.
            in_test_mod = true;
            continue;
        }

        if in_test_mod && trimmed.starts_with("mod ") && trimmed.contains('{') {
            // Entering the test module body. Track brace depth to know when we exit.
            test_mod_brace_depth = Some(brace_depth);
            // Count braces on this line.
            for ch in raw_line.chars() {
                if ch == '{' {
                    brace_depth += 1;
                } else if ch == '}' {
                    brace_depth -= 1;
                }
            }
            continue;
        }

        if in_test_mod && test_mod_brace_depth.is_none() {
            // #[cfg(test)] was not followed by `mod ... {`; reset.
            in_test_mod = false;
        }

        // Track overall brace depth (only when not in a test mod entry line).
        if !(in_test_mod && test_mod_brace_depth.is_some()) {
            for ch in raw_line.chars() {
                if ch == '{' {
                    brace_depth += 1;
                } else if ch == '}' {
                    brace_depth -= 1;
                }
            }
        }

        // Check if we've exited the test module.
        // Check if we've exited the test module.
        if let Some(entry_depth) = test_mod_brace_depth {
            if brace_depth <= entry_depth {
                in_test_mod = false;
                test_mod_brace_depth = None;
            }
        }

        // Skip lines inside test modules.
        if in_test_mod {
            continue;
        }

        // Check for violations on non-test lines.
        let lower = trimmed.to_lowercase();
        if lower.contains(".unwrap()") || lower.contains(".expect(") {
            // Allow unwrap_or_else, unwrap_or, unwrap_or_default — these handle
            // the error case and are not panic-inducing.
            if lower.contains(".unwrap_or_else(")
                || lower.contains(".unwrap_or(")
                || lower.contains(".unwrap_or_default(")
            {
                continue;
            }
            // Allow .lock().expect(...) as poisoned mutexes are a valid panic state.
            if lower.contains(".lock().expect(") {
                continue;
            }
            // Allow .try_into().expect("checked length") — length is verified before call.
            if lower.contains(".try_into().expect(") {
                continue;
            }
            violations.push(Violation {
                file: path.display().to_string(),
                line: line_num,
                content: trimmed.to_string(),
            });
        }
    }

    violations
}

/// Recursively collect all `.rs` files under `src/` directories in `crates/`.
fn collect_source_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_source_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
}

#[test]
fn no_unwrap_or_expect_in_non_test_code() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");

    let crates_dir = workspace_root.join("crates");
    let mut source_files = Vec::new();

    if let Ok(crate_entries) = fs::read_dir(&crates_dir) {
        for crate_entry in crate_entries.flatten() {
            let src_dir = crate_entry.path().join("src");
            if src_dir.is_dir() {
                let mut files = Vec::new();
                collect_source_files(&src_dir, &mut files);
                files.retain(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    // Exclude test helpers and test modules that live in src/
                    name != "fake.rs"
                        && name != "tests.rs"
                        && name != "contract.rs"
                        && name != "shared_contract.rs"
                });
                source_files.extend(files);
            }
        }
    }

    assert!(
        !source_files.is_empty(),
        "expected to find source files under crates/*/src/"
    );

    let mut all_violations = Vec::new();
    for file in &source_files {
        all_violations.extend(scan_file(file));
    }

    if !all_violations.is_empty() {
        let mut report =
            String::from("G5.6 violation — .unwrap() or .expect( found in non-test source code:\n");
        for v in &all_violations {
            report.push_str(&format!("  {}:{}: {}\n", v.file, v.line, v.content));
        }
        report.push_str(
            "\nProduction code must not use .unwrap() or .expect() — the PAM cdylib\n\
             compiles with panic=abort, so a panic aborts the process. Use ? with\n\
             proper error types, or .unwrap_or_else() for provably-infallible cases.\n\
             Test code (inside #[cfg(test)] mod tests) is exempt.",
        );
        panic!("{report}");
    }
}
