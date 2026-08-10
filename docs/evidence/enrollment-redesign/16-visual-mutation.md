# Task 16 — Visual Mutation Test Transcript

Date: 2026-08-10
Commit subject: `docs(evidence): certify enrollment redesign`
Plan reference: `.omo/plans/namtor-usable-intuitive-enrollment.md` task 16, QA failure

## 1. Objective

Seed one hidden primary action (remove the `_Save` mnemonic from the review
page's Save button) and prove the automated test suite rejects it before
reverting. This demonstrates the review and test suite are not vacuous: a real
accessibility regression in a primary action is caught.

## 2. Mutation

File: `crates/namtor-gnome/src/navigation.rs`
Line: 242
Change: `.label(tr("_Save"))` was changed to `.label(tr("Save"))`

This removes the keyboard mnemonic from the Save button, which is the primary
action on the review page. Keyboard users can no longer activate Save with
`Alt+S`. The button is still visible and clickable, but its mnemonic is hidden,
which is the mutation the task asks for.

The `use_underline(true)` call was left in place. The mutation is the missing
underscore prefix, not the property toggle.

## 3. Baseline (before mutation)

```
$ cargo test -p namtor-gnome --test shell_navigation -- primary_actions_have_mnemonics
...
test primary_actions_have_mnemonics ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out
```

The test passes on clean source. All five primary actions (`_Start`, `_Stop`,
`_Save`, `_Resume`, `_Discard`) have mnemonics.

## 4. Mutation applied

The edit:

```
-            .label(tr("_Save"))
+            .label(tr("Save"))
```

## 5. Test result with mutation

```
$ cargo test -p namtor-gnome --test shell_navigation -- primary_actions_have_mnemonics
   Compiling namtor-gnome v0.1.0 (/home/finley/Code/Nam-tor/crates/namtor-gnome)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.75s
     Running tests/shell_navigation.rs (target/debug/deps/shell_navigation-469d26b843c1d5a3)

running 1 test

thread 'primary_actions_have_mnemonics' (153427) panicked at crates/namtor-gnome/tests/shell_navigation.rs:413:9:
primary action 'Save' must have a mnemonic (_Save)
note: run with `RUST_BACKTRACE=1` environment variables to display a backtrace
test primary_actions_have_mnemonics ... FAILED

failures:
    primary_actions_have_mnemonics

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 20 filtered out
```

The test caught the mutation. The failure message names the exact regression:
`primary action 'Save' must have a mnemonic (_Save)`.

## 6. Reversion

The edit was reverted:

```
-            .label(tr("Save"))
+            .label(tr("_Save"))
```

## 7. Test result after reversion

```
$ cargo test -p namtor-gnome --test shell_navigation -- primary_actions_have_mnemonics
    Finished `test` profile [unoptimized + debug] target(s) in 1.65s
     Running tests/shell_navigation.rs (target/debug/deps/shell_navigation-469d26b843c1d5a3)

running 1 test
test primary_actions_have_mnemonics ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out
```

## 8. Clean working tree confirmation

```
$ git diff -- crates/namtor-gnome/src/navigation.rs
(empty)
```

No GNOME source changes remain in the working tree after the mutation test.
The only files staged for this commit are the evidence files under
`docs/evidence/enrollment-redesign/16-*` and `docs/evidence/status-audit.md`.

## 9. Cross-check: existing mutation tests

Task 13 already ships two in-test mutation suites that cover the same class of
regression:

- `mutation_unlabeled_button_rejected`: blanks the `save_button` accessible
  name in an in-memory copy of the registry and asserts the scan flags it.
- `mutation_forbidden_word_rejected`: injects each of the 16 forbidden
  substrings into a clean UI string and asserts the privacy scan catches every
  one.

The task 16 mutation test above is a source-level mutation (edit, compile, test,
revert), which complements the task 13 in-test mutations by proving the test
catches a real source change, not just an in-memory copy.

## 10. Summary

| Step | Result |
|---|---|
| Baseline test (clean source) | PASS |
| Mutation applied (`_Save` to `Save`) | Source edited |
| Test with mutation | FAILED (caught: "primary action 'Save' must have a mnemonic (_Save)") |
| Mutation reverted | Source restored |
| Test after reversion | PASS |
| Working tree clean | Yes (no GNOME source diff) |

The review and test suite reject the hidden primary action mutation. No
blocking finding remains.
