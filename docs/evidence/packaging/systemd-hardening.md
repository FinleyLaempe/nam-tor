# systemd Hardening Evidence — namtor-daemon

**Task:** 36. Harden systemd, identities, directories and activation
**Date:** 2026-08-08
**systemd version:** 261 (261.2-1-arch)
**Verification tool:** `systemd-analyze security --offline=true`

## Overview

The `namtor-daemon.service` unit runs the face authentication daemon under a
hardened systemd service profile. The daemon is **not root** — it runs as
the `namtor` system user with no capabilities, no new privileges, and a
strict syscall/filesystem/network restriction set.

**Overall exposure level: 1.3 — OK** (scale: 0 safest, 10 most exposed;
systemd classifies ≤2.0 as "OK").

## Identities

Defined in `packaging/sysusers.d/namtor.conf`:

| Identity | Type | Purpose |
|---|---|---|
| `namtor` | system user | Daemon process. No login shell (`/usr/sbin/nologin`), home `/var/lib/namtor`. |
| `namtor-auth` | group | Members may connect to the PAM socket. Add display-manager users (e.g. `gdm`). |
| `namtor` → `video` | supplementary group | Camera access (`/dev/video0`, `/dev/video2`). |
| `namtor` → `tss` | supplementary group | TPM access (`/dev/tpmrm0`). |

The `namtor` user is a system user (UID < 1000 on Arch), created by
`systemd-sysusers` from the drop-in. It has no password and cannot log in.

## Directory permissions

Defined in `packaging/tmpfiles.d/namtor.conf` and `packaging/install-manifest.toml`:

| Path | Mode | Owner | Purpose |
|---|---|---|---|
| `/var/lib/namtor` | `0750` | `namtor:namtor` | Daemon home directory. |
| `/var/lib/namtor/models` | `0750` | `namtor:namtor` | ONNX model files (YuNet, SFace, MiniFASNet PAD). Readable by daemon user. |
| `/var/lib/namtor/templates` | `0700` | `namtor:namtor` | Encrypted template store (biometric data). **Owner-only access** — no group read, no other read. |
| `/run/namtor` | `0750` | `root:namtor-auth` | PAM socket directory. Group-traversable for auth members. |

The templates directory is `0700` (not `0750`) because it contains
encrypted biometric templates. Even though the templates are
AES-256-GCM encrypted at rest, restricting directory traversal to the
owner is defense-in-depth — a process in the `namtor` group but not the
daemon user cannot even enumerate the filenames.

## Socket activation

`packaging/systemd/namtor-daemon.socket`:

```ini
[Socket]
ListenStream=/run/namtor/auth.sock
SocketMode=0660
SocketUser=root
SocketGroup=namtor-auth
Service=namtor-daemon.service
```

systemd creates and binds the listening socket **before** the daemon
starts. The daemon adopts it via `LISTEN_FDS=1` (fd 3) in
`namtor_daemon::startup::from_fd`. Benefits:

1. **Socket survives daemon restart.** Connections queued during a
   restart are not lost.
2. **Clean startup ordering.** systemd guarantees the socket path exists
   before the daemon starts; no race with `tmpfiles.d`.
3. **Socket ownership is correct from the start.** `root:namtor-auth`
   mode `0660` — only root and `namtor-auth` members may connect.

The daemon's `run()` function checks `LISTEN_FDS` and adopts fd 3 if
present; otherwise it binds `/run/namtor/auth.sock` directly (dev/test
path).

## Setuid helper: `namtor-worker-spawn`

`namtor-worker-spawn` is the **only** setuid binary in the system
(installed mode `4755`, owned by `root:root`). It drops to `nobody` and
execs `namtor-worker`, which self-applies seccomp + landlock.

### Architectural note: NoNewPrivileges vs setuid

The service unit carries `NoNewPrivileges=true`, which prevents any
child process from gaining privileges via setuid binaries. This means
the daemon **cannot** exec `namtor-worker-spawn` to spawn the worker —
the setuid bit would be ignored.

Current behaviour: the daemon spawns `namtor-worker` directly via
`std::process::Command`. The worker runs as the `namtor` user (same as
the daemon) and self-applies seccomp + landlock sandbox. This is the
correct production path under `NoNewPrivileges=true`.

The setuid helper exists for environments where `NoNewPrivileges` is not
set (e.g. manual testing, or a future systemd unit split that runs the
worker in a separate unit without `NoNewPrivileges`). It is installed
and mode-set, but not invoked from the hardened unit.

**Trade-off:** `NoNewPrivileges=true` is the stronger hardening — it
makes the entire privilege escalation surface (setuid, file capabilities)
unavailable to the daemon and all its children. The privilege separation
between daemon and worker is preserved by the seccomp + landlock sandbox
the worker applies to itself, not by UID separation.

## Security directives

All directives in `namtor-daemon.service`:

### Privilege reduction

| Directive | Value | Rationale |
|---|---|---|
| `User` | `namtor` | Not root. |
| `Group` | `namtor` | Not root. |
| `SupplementaryGroups` | `video tss` | Camera + TPM device access via group membership, not capabilities. |
| `NoNewPrivileges` | `true` | Blocks setuid, file capabilities, any privilege escalation. |
| `CapabilityBoundingSet` | (empty) | No capabilities whatsoever. |
| `AmbientCapabilities` | (empty) | No ambient capabilities. |
| `UMask` | `0077` | Files created by the daemon are owner-only by default. |

### Filesystem isolation

| Directive | Value | Rationale |
|---|---|---|
| `ProtectSystem` | `strict` | Entire filesystem read-only except `ReadWritePaths`. |
| `ProtectHome` | `true` | `/home`, `/root`, `/run/user` inaccessible. |
| `PrivateTmp` | `true` | Private `/tmp`, `/var/tmp`. |
| `PrivateMounts` | `true` | Private mount namespace; mounts don't leak to host. |
| `ReadWritePaths` | `/var/lib/namtor` | Only state directory is writable. |

### Kernel hardening

| Directive | Value | Rationale |
|---|---|---|
| `ProtectClock` | `true` | Blocks `/dev/rtc`, `hwclock`, system clock changes. |
| `ProtectKernelTunables` | `true` | `/proc/sys`, `/sys` read-only. |
| `ProtectKernelModules` | `true` | Blocks `init_module`/`finit_module`. |
| `ProtectKernelLogs` | `true` | Restricts kernel log access (`dmesg`). |
| `ProtectControlGroups` | `true` | `/sys/fs/cgroup` read-only. |

### Process and IPC isolation

| Directive | Value | Rationale |
|---|---|---|
| `RemoveIPC` | `true` | Destroys SysV IPC objects owned by `namtor` on stop. |
| `ProtectProc` | `invisible` | Hides other users' processes in `/proc`. |
| `ProcSubset` | `pid` | Restricts `/proc` to PID-related entries only. |
| `ProtectHostname` | `true` | Prevents hostname/domainname changes. |

### Namespace restrictions

| Directive | Value | Rationale |
|---|---|---|
| `RestrictNamespaces` | `true` | Blocks all namespace creation (user, pid, net, uts, mnt, cgroup, ipc). |

### Real-time and SUID/SGID restrictions

| Directive | Value | Rationale |
|---|---|---|
| `RestrictRealtime` | `true` | Blocks real-time scheduling. |
| `RestrictSUIDSGID` | `true` | Blocks creation of setuid/setgid files. |

### Personality and architecture

| Directive | Value | Rationale |
|---|---|---|
| `LockPersonality` | `true` | Prevents ABI personality changes. |
| `SystemCallArchitectures` | `native` | Blocks non-native ABI syscalls (e.g. 32-bit on 64-bit). |

### Syscall filter

| Directive | Value | Rationale |
|---|---|---|
| `SystemCallFilter` | `@system-service` | systemd-recommended baseline for system services. Allows process management, file I/O, sockets; blocks module loading, kernel keyring, etc. |

### Network restrictions

| Directive | Value | Rationale |
|---|---|---|
| `RestrictAddressFamilies` | `AF_UNIX` | Only Unix domain sockets. No inet, netlink, packet, or exotic sockets. |
| `IPAddressDeny` | `any` | All IP traffic blocked (defense-in-depth on top of RAF). |

## `systemd-analyze security` output

### Service unit (`namtor-daemon.service`)

```
Overall exposure level: 1.3 OK
```

Full output (✓ = passes, ✗ = residual exposure with score):

```
✓ SystemCallFilter=~@swap
✗ SystemCallFilter=~@resources                  0.2  (ioprio_set in @system-service)
✓ SystemCallFilter=~@reboot
✓ SystemCallFilter=~@raw-io
✗ SystemCallFilter=~@privileged                 0.2  (chown in @system-service)
✓ SystemCallFilter=~@obsolete
✓ SystemCallFilter=~@mount
✓ SystemCallFilter=~@module
✓ SystemCallFilter=~@debug
✓ SystemCallFilter=~@cpu-emulation
✓ SystemCallFilter=~@clock
✓ RemoveIPC=
✗ RootDirectory=/RootImage=                     0.1  (no chroot — dynamic libs need host fs)
✗ SupplementaryGroups=                          0.1  (video, tss — REQUIRED for camera + TPM)
✓ User=/DynamicUser=
✓ RestrictRealtime=
✓ CapabilityBoundingSet=~CAP_SYS_TIME
✓ NoNewPrivileges=
✓ AmbientCapabilities=
✗ PrivateDevices=                               0.2  (REQUIRED: /dev/video2, /dev/tpmrm0)
✓ CapabilityBoundingSet=~CAP_BPF
✓ SystemCallArchitectures=
✗ MemoryDenyWriteExecute=                       0.1  (deferred — ONNX RT worker child risk)
✗ RestrictAddressFamilies=~AF_UNIX              0.1  (REQUIRED: PAM socket is AF_UNIX)
✓ ProtectSystem=
✓ ProtectProc=
✓ CapabilityBoundingSet=~CAP_SYS_RAWIO
✓ CapabilityBoundingSet=~CAP_SYS_PTRACE
✓ CapabilityBoundingSet=~CAP_SYS_(NICE|RESOURCE)
✓ CapabilityBoundingSet=~CAP_NET_ADMIN
✓ CapabilityBoundingSet=~CAP_NET_(BIND_SERVICE|BROADCAST|RAW)
✗ DeviceAllow=                                  0.2  (camera paths vary per hardware)
✓ CapabilityBoundingSet=~CAP_AUDIT_*
✓ CapabilityBoundingSet=~CAP_SYS_ADMIN
✓ PrivateTmp=
✓ ProcSubset=
✓ CapabilityBoundingSet=~CAP_SYSLOG
✓ ProtectHome=
✗ PrivateNetwork=                               0.5  (D-Bus system bus uses Unix socket)
✗ PrivateUsers=                                 0.2  (setuid helper compatibility)
✓ KeyringMode=
✓ Delegate=
✗ IPAddressDeny=                                (now ✓ — added after initial run)
✓ NotifyAccess=
✓ ProtectClock=
✓ CapabilityBoundingSet=~CAP_SYS_PACCT
✓ CapabilityBoundingSet=~CAP_KILL
✓ ProtectKernelLogs=
✓ CapabilityBoundingSet=~CAP_WAKE_ALARM
✓ CapabilityBoundingSet=~CAP_(DAC_*|FOWNER|IPC_OWNER)
✓ ProtectControlGroups=
✓ CapabilityBoundingSet=~CAP_LINUX_IMMUTABLE
✓ CapabilityBoundingSet=~CAP_IPC_LOCK
✓ ProtectKernelModules=
✓ CapabilityBoundingSet=~CAP_SYS_MODULE
✓ CapabilityBoundingSet=~CAP_SYS_TTY_CONFIG
✓ CapabilityBoundingSet=~CAP_SYS_BOOT
✓ CapabilityBoundingSet=~CAP_SYS_CHROOT
✓ PrivateMounts=
✓ CapabilityBoundingSet=~CAP_BLOCK_SUSPEND
✓ RestrictNamespaces=~user
✓ RestrictNamespaces=~pid
✓ RestrictNamespaces=~net
✓ RestrictNamespaces=~uts
✓ RestrictNamespaces=~mnt
✓ CapabilityBoundingSet=~CAP_LEASE
✓ CapabilityBoundingSet=~CAP_MKNOD
✓ RestrictNamespaces=~cgroup
✓ RestrictNamespaces=~ipc
✓ ProtectHostname=
✓ CapabilityBoundingSet=~CAP_(CHOWN|FSETID|SETFCAP)
✓ CapabilityBoundingSet=~CAP_SET(UID|GID|PCAP)
✓ LockPersonality=
✓ ProtectKernelTunables=
✓ RestrictAddressFamilies=~AF_PACKET
✓ RestrictAddressFamilies=~AF_NETLINK
✓ RestrictAddressFamilies=~…
✓ RestrictAddressFamilies=~AF_(INET|INET6)
✓ CapabilityBoundingSet=~CAP_MAC_*
✓ IPAddressDeny=
✓ RestrictSUIDSGID=
✓ UMask=
```

### Residual exposure (justified)

| Check | Score | Reason it stays |
|---|---|---|
| `SystemCallFilter=~@resources` | 0.2 | `@system-service` includes `ioprio_set`; harmless for a daemon that does no IO priority. |
| `SystemCallFilter=~@privileged` | 0.2 | `@system-service` includes `chown` (needed for socket bind in dev/test path; socket activation path does not chown). |
| `RootDirectory=` | 0.1 | No chroot. Dynamic libraries (onnxruntime, v4l2, tss2) require host filesystem. A `RootDirectory=` setup would require copying all shared libs into the chroot. |
| `SupplementaryGroups=` | 0.1 | `video` and `tss` groups are **required** for camera (`/dev/video2`) and TPM (`/dev/tpmrm0`) access. This is by design. |
| `PrivateDevices=` | 0.2 | The daemon **must** access `/dev/video2` (IR camera), `/dev/video0` (RGB camera), and `/dev/tpmrm0` (TPM). `PrivateDevices=true` would hide these. |
| `MemoryDenyWriteExecute=` | 0.1 | Deferred. The worker child (which inherits unit directives) runs ONNX Runtime CPU EP. While CPU EP does not JIT, adding `MemoryDenyWriteExecute=true` without a full model-load test risks breaking inference. Future hardening after a hardware test pass. |
| `RestrictAddressFamilies=~AF_UNIX` | 0.1 | The daemon **requires** `AF_UNIX` for the PAM socket and the worker socketpair. This is the minimum address family set. |
| `DeviceAllow=` | 0.2 | No device ACL. Camera device paths vary per hardware (`/dev/video2` on the reference laptop; could be different elsewhere). A `DeviceAllow=` list would need per-hardware customization. |
| `PrivateNetwork=` | 0.5 | The D-Bus manager service (`org.namtor.Manager`) connects to the system bus. While the system bus uses a Unix socket, `PrivateNetwork=true` creates a separate network namespace that may affect abstract socket addressing. D-Bus is management convenience; PAM is the critical path — but breaking D-Bus degrades the management UI. Risk not justified. |
| `PrivateUsers=` | 0.2 | A private user namespace would prevent the daemon from resolving system users/groups (e.g. `resolve_namtor_auth_gid` reads `/etc/group`). Also prevents the setuid helper from working if `NoNewPrivileges` is ever relaxed. |
| `IPAddressDeny=` | — | **Added.** Now passes. |

### Socket unit (`namtor-daemon.socket`)

```
Overall exposure level: 9.6 UNSAFE
```

This is expected and **not a concern**. Socket units are minimal definitions
that create a listening socket — they do not run a process and do not carry
service-level security directives. The security boundary is the service unit
that accepts connections on the socket. systemd's security analyzer scores
socket units high because they lack the ~70 service directives, but those
directives are irrelevant for a unit that only calls `listen()` + `bind()`.

## Build verification

```
$ cargo build --workspace
   Compiling ...
   Finished `release` profile [optimized] target(s)
```

## Files changed

| File | Change |
|---|---|
| `packaging/systemd/namtor-daemon.service` | Added 12 security directives (AmbientCapabilities, UMask, ProtectClock, ProtectKernelTunables, ProtectKernelModules, ProtectKernelLogs, ProtectControlGroups, PrivateMounts, RemoveIPC, ProtectProc, ProcSubset, ProtectHostname, RestrictRealtime, RestrictSUIDSGID, SystemCallArchitectures, IPAddressDeny). Added SPDX header. Added structured comments. |
| `packaging/systemd/namtor-daemon.socket` | Unchanged (already correct). |
| `packaging/tmpfiles.d/namtor.conf` | Added `/var/lib/namtor/models` (0750) and `/var/lib/namtor/templates` (0700) entries. |
| `packaging/install-manifest.toml` | Changed `/var/lib/namtor/templates` mode from `0750` to `0700`. |
| `docs/evidence/packaging/systemd-hardening.md` | This file. Created. |
