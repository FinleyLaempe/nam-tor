# GNOME Shell QA Evidence — Async Shell & Startup States

## 1. Async Architecture
The GNOME UI has been refactored to prevent main-thread blocking during D-Bus I/O.

### Implementation
- **Non-blocking calls**: D-Bus interactions (ListProfiles, GetStatus) are transitioned to a background-executor pattern. 
- **View-Model integration**: `PresenterModel` now includes an `is_loading` state.
- **Main-thread safety**: The GTK main loop is never blocked. All results are delivered back to the UI thread via `glib::MainContext` or the presenter's state update cycle.

## 2. Startup States Verification

| State | Trigger | Visual Evidence | Result |
|---|---|---|---|
| **Loading** | Application launch / Refresh | `status_label` shows "Loading..." | PASS |
| **Ready** | Successful D-Bus connection | Profile list renders; status "up" | PASS |
| **Empty** | Connection success, 0 profiles | "No profiles enrolled yet." label displayed | PASS |
| **Error** | Daemon unavailable | `RecoveryView` rendered with actionable steps | PASS |

## 3. Resiliency Testing

### Delayed Service
- **Symptom**: D-Bus call takes > 500ms.
- **Behavior**: The UI remains responsive. The "Loading..." state persists until the timeout or response arrives. The window can still be moved/resized.
- **Verdict**: PASS (No freeze).

### Failing Service
- **Symptom**: D-Bus returns `ServiceUnknown` or `DaemonUnavailable`.
- **Behavior**: The shell transitions immediately to the `RecoveryView` state. The error is caught in the presenter and mapped to a `RecoveryHint`.
- **Verdict**: PASS (Graceful degradation).
