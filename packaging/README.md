# Nam-tor packaging

Systemd units, sysusers/tmpfiles drop-ins, and the setuid worker-spawn helper.

## Layout

| File | Purpose |
|---|---|
| `systemd/namtor-daemon.socket` | Socket-activated PAM socket (`/run/namtor/auth.sock`, mode 0660, owner `root:namtor-auth`) |
| `systemd/namtor-daemon.service` | Hardened daemon unit (user `namtor`, groups `video tss`, no caps, `ProtectSystem=strict`) |
| `sysusers.d/namtor.conf` | Creates user `namtor` + group `namtor-auth`, adds `namtor` to `video`/`tss` |
| `tmpfiles.d/namtor.conf` | Creates `/var/lib/namtor` (model store) and `/run/namtor` (socket dir) |

The setuid helper binary is `namtor-worker-spawn` (built from
`crates/namtor-worker/src/bin/namtor-worker-spawn.rs`).

## Install (Arch Linux; adapt paths for other distros)

```sh
# 1. Build and install binaries
cargo build --release -p namtor-daemon -p namtor-worker -p namtor-cli -p namtor-camera-emitter-off
install -Dm755 target/release/namtor-daemon         /usr/lib/namtor/namtor-daemon
install -Dm755 target/release/namtor-worker         /usr/lib/namtor/namtor-worker
install -Dm4755 target/release/namtor-worker-spawn  /usr/lib/namtor/namtor-worker-spawn  # setuid root
install -Dm755 target/release/namtor-cli            /usr/bin/namtor-cli
install -Dm755 target/release/namtor-camera-emitter-off /usr/lib/namtor/namtor-camera-emitter-off

# 2. System units + drop-ins
install -Dm644 packaging/systemd/namtor-daemon.service /usr/lib/systemd/system/namtor-daemon.service
install -Dm644 packaging/systemd/namtor-daemon.socket  /usr/lib/systemd/system/namtor-daemon.socket
install -Dm644 packaging/sysusers.d/namtor.conf        /usr/lib/sysusers.d/namtor.conf
install -Dm644 packaging/tmpfiles.d/namtor.conf        /usr/lib/tmpfiles.d/namtor.conf

# 3. Create users, groups, and runtime directories
sudo systemd-sysusers /usr/lib/sysusers.d/namtor.conf
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/namtor.conf

# 4. Add display-manager user (e.g. gdm) to namtor-auth
sudo gpasswd -a gdm namtor-auth

# 5. Enable and start
sudo systemctl enable --now namtor-daemon.socket
sudo systemctl start namtor-daemon.service
```

## Verifying

```sh
systemctl status namtor-daemon.service
namtor-cli status
```

`namtor-cli status` should show `stream: closed` immediately after daemon
start (warm worker, cold camera — spec §5.5).

## Security notes

- `namtor-worker-spawn` is the **only** setuid binary. It drops to `nobody`
  and execs `namtor-worker`, which self-applies seccomp + landlock.
- The daemon (`namtor` user) is **not root** and has no capabilities
  (`CapabilityBoundingSet=` empty).
- `namtor-auth` group membership controls who may connect to the PAM socket.
  Add display-manager users (e.g. `gdm`) to it; do not add regular users.
- `SO_PEERCRED` is checked on every connection (spec §3.2); remote
  authentication (`PAM_RHOST` set) is always refused (spec §8.2).
