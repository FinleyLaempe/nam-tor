# Nam-tor — Handoff

> **Experimental preview.** Do not enable on security-sensitive PAM services.
> No G1-G5 production gate has passed with measured evidence yet.

This file was the original cold-start entry point. Project memory now lives in
[`AGENTS.md`](AGENTS.md) — read that first, then the
[spec](docs/superpowers/specs/2026-08-04-nam-tor-design.md) and
[roadmap](docs/superpowers/plans/2026-08-04-nam-tor-roadmap.md).

M0-M9 are complete and committed on the reference hardware. M10 (presence,
depth, optional backbones) is implemented. No G1-G5 production gate is
claimed as passed; the authoritative readiness ledger is
[`docs/evidence/status-audit.md`](docs/evidence/status-audit.md). A
production-readiness program (`.omo/plans/namtor-complete-product.md`)
is underway to close the remaining gates with measured evidence.

Nam-tor ships as three split packages (`namtor`, `namtor-gnome`,
`namtor-kde`) on Arch, Fedora, and Debian. PAM services are off by default
at install time. Setup is Settings-only: open the Settings app, enroll, and
choose PAM targets. No CLI or root shell is needed. See
[`packaging/README.md`](packaging/README.md) for the packaging layout and
[`README.md`](README.md) for the public-facing summary.

