# Contributing to Nam-tor

## The most valuable contribution is a hardware report

Nam-tor's zero-configuration promise depends on the device database in [`quirks/`](quirks/). Microsoft ships a per-device driver metadata store; we have a community database instead. **It only works if it has entries.**

If you have a laptop with an IR camera:

```bash
namtor-probe --contribute
```

This detects your IR sensor, probes for emitter control, verifies it optically, and prints the exact `gh pr create` command to submit the result. It takes about a minute.

Entries record hardware facts only — USB identifiers, interface numbers, format GUIDs, extension unit parameters. **No personal data, no images.** The database is Apache-2.0 so any project can use it, including Howdy and, ideally, the kernel.

A report from a camera that *doesn't* work is just as useful as one that does.

## Before writing code

Read, in order:

1. [`docs/superpowers/specs/2026-08-04-nam-tor-design.md`](docs/superpowers/specs/2026-08-04-nam-tor-design.md) — the specification
2. [`docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md`](docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md) — milestone structure
3. The plan for the milestone you are working on

Then run `./scripts/check-env.sh`. It reports what your machine is missing and which milestone each gap blocks.

## How the work is structured

Implementation follows written plans with bite-sized TDD tasks: write the failing test, run it and watch it fail, write the minimal implementation, run it and watch it pass, commit.

This is not ceremony. The project makes measurable claims about false-accept rates and spoof resistance, and a test written after the implementation tends to assert what the code does rather than what it should do.

**Milestone plans are written after their blocking measurements land.** M2 through M10 are scoped in the roadmap but not decomposed into tasks, because their details depend on hardware results from M1. Planning against unmeasured assumptions produces fiction.

## Non-negotiables

These are enforced by CI, not by review:

- **SPDX header on every source file.** See [`COPYING.md`](COPYING.md) for which licence applies where.
- **No model weights, no datasets, ever committed.** See [`ml/DATASETS.md`](ml/DATASETS.md).
- **No biometric data in logs, at any level.** No embeddings, no images, no template contents.
- **The PAM module contains no ML code**, no ONNX, no image decoders, and fails closed on every error path.
- **Probe safety** (spec §4.3): never write an extension unit control you have not read; always round-trip and restore; honour `GET_LEN` and `GET_INFO`; skip controls without SET support. `SafeXu` makes these structurally unavoidable — do not route around it.

## Things that look like improvements but are not

Each of these has been considered and rejected for a reason recorded in the spec:

| Tempting change | Why not |
|---|---|
| Add a GPU execution provider to speed up auth | The dGPU sits at D3cold under runtime PM; waking it costs ~1 s, more than the entire latency budget. CPU INT8 already fits. And a GPU EP punches holes in the worker sandbox. |
| Expose a threshold slider in the GUI | A user who types `0.4` into a box has silently disabled their own security. Named operating points only, each labelled with its measured FAR/FRR. |
| Make PAD a weighted term in the decision | It is a **veto gate**. A successful spoof produces a very strong match by construction, so a weighted sum lets a good spoof outvote its own detection. |
| Prompt before MSXU probing, for safety | MSXU has published control semantics — it is not a blind write. The prompt exists for vendor-unit payload search, where the risk is real. Prompting on MSXU costs the zero-interaction property (gate G1.4) and buys nothing. |
| Use a bigger backbone to improve accuracy | R50 → R100 doubles cost for less gain than fixing enrollment quality and adding score normalisation, both of which are free. Check the ablation table first. |
| Lower the bar in darkness when RGB is unavailable | Then "attack in a dark room" becomes the trivial bypass. Fusion may add confidence, never subtract it. |

If you think one of these is wrong, say so with reasoning — they are decisions, not commandments. Just don't change them silently.

## Claims and evidence

The project publishes error rates. Two rules govern them:

- **No rate without its sample size.** A FAR of 1e-5 measured on one subject is not a measurement. The report renderer enforces this and a test asserts the enforcement.
- **APCER per attack type, never averaged.** An averaged figure conceals precisely the attack that works.

If a measurement misses an acceptance gate, report the number and scope the claim. Do not soften the gate.

## Security reports

Report suspected vulnerabilities privately rather than opening a public issue.

Reports concerning the socket ACL (spec §3.2) deserve particular attention: anyone who can assert *"authenticate user X"* and be believed has defeated every other control in the system.

## Code of conduct

Be decent. Assume good faith. Argue about the work, not the person.
