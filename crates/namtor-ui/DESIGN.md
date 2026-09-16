# Nam-tor UI Design System

This document defines the visual and interaction language for the Nam-tor management UI (`namtor-ui`). The app is GTK4 + `libadwaita` and follows the GNOME Human Interface Guidelines (HIG) because that is libadwaita's design language — not because the app is GNOME-only. It has no GNOME-specific runtime dependency and ships as the single, desktop-neutral UI.

## 0. Research Log

- **GNOME HIG**: Studied official GNOME HIG patterns for adaptive navigation, particularly `AdwViewSwitcher` / `AdwViewSwitcherBar` for settings applications
- **libadwaita patterns**: Analyzed GNOME Settings, Boxes, and other libadwaita applications for three-destination patterns with drill-down enrollment flows
- **Existing project patterns**: Built upon the v3 enrollment state machine from task 7 (Setup → Capturing → Stopped → Reviewing → Saving → Error)

## 1. Design Philosophy

- **Native & Seamless**: Use Adwaita components strictly. Avoid custom "brand" styles that clash with the host desktop.
- **Adaptive First**: The UI adapts to available space — narrow windows (< 600px) use a bottom switcher bar; wide windows (≥ 600px) use a title-bar switcher.
- **Atomic Interaction**: Every operation (enrollment, deletion, etc.) must have a clear start, progress, and resolution state.
- **Safe by Default**: Biometric data is never surfaced. Feedback is coarse (e.g., "High Quality") to maintain privacy.
- **Resilient**: The UI must remain responsive even if the `org.namtor.Manager` daemon is slow or unavailable.
- **No Custom CSS**: All styling comes from libadwaita defaults. Custom CSS is forbidden.

## 2. Visual Tokens

### 2.1 Typography (Adwaita Scale)
| Role | Adwaita Class | Purpose |
|---|---|---|
| Title | `.title` | Page titles, section headers |
| Heading | `.heading` | Subsection headers |
| Body | (default) | Profile names, status messages |
| Caption | `.caption` | Timestamps, metadata, daemon version |
| Dimmed | `.dim-label` | Secondary information, inactive states |
| Error | `.error` | Critical failures, daemon unavailable |
| Warning | `.warning` | Low quality, needs re-enrollment |
| Success | `.success` | Operation completed successfully |

### 2.2 Spacing & Layout
- **Base Grid**: 8px / 4px.
- **Margins**: 12px for main content windows; 24px for centered recovery pages.
- **Gaps**: 4px for related elements (e.g., within a profile row), 8px for layout blocks.
- **Alignment**: All labels are `halign(Align::Start)` by default.
- **Adaptive Breakpoint**: 600px — below this width, the navigation switches from title-bar switcher to bottom switcher bar.

### 2.3 Components

#### Profile Row
- **Structure**: `[Name (expands)] [Enabled Toggle] [Last Used (dimmed)]`
- **Interaction**: Row is an `AccessibleRole::ListItem`. Toggle controls the active status.
- **No biometric data**: Never show raw scores, embeddings, or template metadata.

#### Enrollment Capture Page
- **Container**: `AdwPreferencesPage` with four groups (hero, signals, counters, actions).
- **Hero** (untitled group):
  - Face silhouette in a `.card` frame — the stand-in for the camera view.
    **No camera preview** — privacy by design. Its tint follows
    `quality_bucket` (high `.success`, medium `.dim-label`, low `.warning`).
  - Guidance headline in `.title-1` — the largest text on the page, because
    it is the only actionable element. Styled from the *typed* guidance and
    the quality bucket, never by matching the translated label.
  - One dimmed line under it saying concretely how to act on the guidance.
  - When the scan is complete the headline becomes "Enrollment complete"
    rather than continuing to ask for more.
- **Scan quality** group — the per-signal breakdown:
  - "Image quality" (`quality_percent`, tinted by `quality_bucket`)
  - "Angle variety" (`diversity_percent`)
  - "Views kept" (`frames_accepted / frames_seen`, or "—" before the first view)
  - "Needs attention" — the signal the guidance token implicates
    (Framing / Lighting / Steadiness / Head angles / Nothing)
- **Views this session** group: distinct Accepted / Seen / Rejected rows.
- **Primary Action**: "Stop and review" (`.suggested-action .pill`), Cancel below it.

#### Enrollment Review Page
- **Container**: `AdwPreferencesPage` with hero, signals and actions groups.
- **Hero**: the same silhouette treatment; headline is the verdict
  ("Ready to save" / "Not ready to save yet") with the daemon's guidance and
  its how-to rendered underneath. The review screen must never drop
  `guidance`.
- **What the scan collected**: Image quality, Angle variety, Needs attention,
  Accepted.
- **Actions**: the accent goes to the action that is actually right — Save
  when `ready_to_save`, Resume otherwise. Discard is `.destructive-action .flat`.

#### Enrollment Setup Page
- **Container**: `AdwPreferencesGroup` with entry field.
- **Content**:
  - Profile name entry (with placeholder "Enter profile name…")
  - Privacy explanation: "Continuous capture mode — frames are analyzed locally, no raw images stored"
- **Primary Action**: Start button

#### Status Banner (Top)
- **Content**: "Daemon {up/down} · Camera {present/absent} · v{version}"
- **Style**: `AdwBanner` with `.dim-label .caption`
- **Placement**: Top of content area, below header bar

#### Error / Recovery Page
- **Layout**: Centered `AdwStatusPage` (icon: `dialog-error-symbolic`).
- **Structure**: Title (Error) → Detail (Symptom) → Recovery Steps (Bullet list).
- **Action**: Clear "Retry" or "Recovery" button providing specific actions.

#### Offline State
- **Layout**: `AdwStatusPage` with `computer-fail-symbolic` or `network-offline-symbolic`.
- **Content**: "Cannot reach the Nam-tor daemon" + detail + recovery steps.
- **Action**: "Retry" button re-attempts D-Bus connection.
- **Never show**: Empty unexplained space, disabled controls without explanation.

## 3. Navigation Architecture

### 3.1 Three Destinations (Settings Shell)

The main settings window has exactly three destinations, managed by an `AdwViewStack`:

| Destination | Icon | Purpose | Default |
|---|---|---|---|
| **Profiles** | `system-users-symbolic` | Manage enrolled face profiles | Yes |
| **Authentication** | `authenticator-symbolic` | PAM service toggles, protection status | No |
| **System** | `emblem-system-symbolic` | Daemon status, camera info, about | No |

### 3.2 Adaptive Navigation Pattern

```
Wide (≥ 600px):
┌─────────────────────────────────────┐
│  [≡] Nam-tor — {Destination}   [?] │ ← HeaderBar with AdwViewSwitcher
├─────────────────────────────────────┤
│                                     │
│           ViewStack Content         │
│                                     │
└─────────────────────────────────────┘

Narrow (< 600px):
┌─────────────────────────────────────┐
│  [≡] Nam-tor            [?]         │ ← HeaderBar (no switcher)
├─────────────────────────────────────┤
│                                     │
│           ViewStack Content         │
│                                     │
├─────────────────────────────────────┤
│ [Profiles] [Authentication] [System]│ ← AdwViewSwitcherBar (bottom)
└─────────────────────────────────────┘
```

**Implementation**:
- `AdwViewStack` holds the three destination pages
- `AdwViewSwitcher` in header bar for wide windows
- `AdwViewSwitcherBar` at bottom for narrow windows
- `AdwBreakpoint` at 600px switches between them
- Window title updates to match active destination

### 3.3 Enrollment Navigation (NavigationView)

The enrollment flow uses `AdwNavigationView` for drill-down navigation:

```
Setup Page → Capture Page → Review Page
   ↑            ↑              ↑
   │            │              │
   └────────────┴──────────────┘
        Back navigation (swipe, button)
```

**Pages**:
1. **Setup**: Enter name, privacy notice, Start button
2. **Capture**: Live progress, guidance, Stop button
3. **Review**: Summary, Save/Resume/Discard buttons

**Back Navigation**:
- Back button in header (automatic via `AdwNavigationPage`)
- Swipe-right gesture on touch devices
- Esc key on keyboard

### 3.4 State Machine & Transitions

| State | Trigger | Visual Representation |
|---|---|---|
| **Loading** | App Start | `AdwStatusPage` with spinner, "Connecting…" |
| **Ready** | D-Bus connected | Profile list rendered; status banner "up" |
| **Unreachable**| D-Bus timeout/error | `AdwStatusPage` with retry action (no list, no toolbar) |
| **Enrolling: Setup** | User clicks "Enroll" | Setup page with name entry |
| **Enrolling: Capturing** | Start clicked | Capture page with progress, Stop button |
| **Enrolling: Stopped** | Stop clicked | Review page with Save/Resume/Discard |
| **Enrolling: Saving** | Save clicked | Spinner overlay, "Saving…" |
| **Enrolling: Error** | Operation failed | Error banner with retry/dismiss |

## 4. Header Bar Conventions

Every page has a consistent header bar with:

- **Window title**: Matches the active destination ("Profiles", "Authentication", "System") or enrollment page ("Enroll Face")
- **One primary action per state**:
  - Profiles: Enroll button (when idle)
  - Capture: Stop button (during enrollment)
  - Review: Save button (primary), Discard (destructive in menu)
  - Offline: Retry button
- **No clutter**: Secondary actions go in menus (kebab menu `view-more-symbolic`)

## 5. Accessibility

- **Accessible roles**: Every control must have a meaningful `accessible_role` (e.g., `ListItem`, `Checkbox`, `Alert`, `Button`).
- **Accessible names**: All navigation controls have `tooltip-text` or `aria-label` for screen readers.
- **Keyboard navigation**: Logical tab traversal (Top → Bottom, Left → Right). Mnemonics on primary actions (`_Start`, `_Stop`, `_Save`).
- **Focus indicators**: libadwaita default focus rings are preserved (no custom CSS to remove them).
- **Screen reader announcements**: Error messages use `Alert` role; progress updates use `StatusBar` role.
- **High contrast**: All text meets WCAG AA contrast ratios (libadwaita default palette ensures this).

## 6. Loading / Offline / Error States

### 6.1 Loading State
- **When**: Daemon connection in progress, profile list fetching
- **Visual**: `AdwStatusPage` with `spinner` icon and "Loading…" text
- **Never**: Empty unexplained space, disabled controls without explanation

### 6.2 Offline State (Daemon Unavailable)
- **When**: D-Bus connection fails, daemon crashes
- **Visual**: `AdwStatusPage` with `computer-fail-symbolic` icon
- **Title**: "Cannot reach the Nam-tor daemon"
- **Detail**: Technical error (e.g., "Connection refused")
- **Recovery**: Bullet list of steps (start daemon, check logs, reinstall)
- **Action**: "Retry" button re-attempts connection
- **Never**: Show the profile list with disabled controls, show empty space

### 6.3 Error State (Recoverable)
- **When**: Enrollment fails, polkit denied, low diversity
- **Visual**: `AdwBanner` at top of content area (`.error` class)
- **Title**: Error title (e.g., "Enrollment failed")
- **Detail**: Brief explanation
- **Recovery**: One actionable step or "Retry" button
- **Dismiss**: Errors can be dismissed; offline state cannot

### 6.4 Empty State (No Profiles)
- **When**: Zero profiles enrolled
- **Visual**: `AdwStatusPage` with `list-add-symbolic` icon
- **Title**: "No profiles enrolled yet"
- **Detail**: "Enroll your face to get started"
- **Action**: "Enroll Face" button
- **Never**: Show an empty list with no explanation

## 7. Keyboard Order

Tab traversal follows visual order:

1. Header bar controls (left to right)
2. Status banner (if present)
3. Main content (top to bottom)
4. Bottom switcher bar (if present, left to right)
5. Window controls

**Mnemonics**:
- `_Enroll` (Setup page Start button)
- `_Stop` (Capture page Stop button)
- `_Save` (Review page Save button)
- `_Retry` (Offline/Retry button)
- `_Discard` (Review page Discard action)

## 8. No Custom CSS

**Policy**: All styling comes from libadwaita defaults. Custom CSS is forbidden because:

1. It breaks consistency with the host desktop
2. It requires maintenance across libadwaita versions
3. It can interfere with accessibility (high contrast, large text modes)
4. libadwaita's default palette already meets WCAG AA

**Allowed**:
- Adding/removing libadwaita CSS classes (e.g., `.error`, `.warning`, `.dim-label`)
- Using libadwaita's built-in components (`AdwStatusPage`, `AdwBanner`, `AdwPreferencesPage`)

**Forbidden**:
- `style.css` files with custom rules
- Inline `css_classes` that invent new design tokens
- Overriding libadwaita variables (e.g., `--accent-color`)

## 9. Component Primitives

### 9.1 Shell (`shell.rs`)
- Owns `AdwApplicationWindow` with `AdwToolbarView`
- Contains `AdwViewStack` with three pages
- Manages `AdwBreakpoint` for adaptive switching
- Provides `set_destination()` and `active_destination()` methods

### 9.2 Navigation
The shell owns the single `AdwNavigationView`; the enrollment and review
sub-pages are pushed onto it via `Shell::push_subpage()` / `pop_subpage()`.
Each sub-page carries its own `AdwToolbarView` + `AdwHeaderBar`, because
`AdwNavigationView` does not supply one.

There is exactly **one** enrollment rendering path. An earlier
`navigation.rs` built a second, divergent set of capture and review widgets
that no production code ever reached; it was removed rather than kept in
sync. `AppController` renders through the components below.

### 9.3 Enrollment Components (`components/enrollment.rs`, `components/review.rs`)
- `EnrollmentViewComponent::render(None, …)`: name entry, privacy notice, Start
- `EnrollmentViewComponent::render(Some(view), …)`: the capture page above
- `EnrollmentViewComponent::render_stopped(…)`: stopped summary + Review/Cancel
- `ReviewViewComponent::render(…)`: the review page above

### 9.3b Scan Feedback (`components/scan_feedback.rs`)
Shared building blocks for both scan pages: `silhouette()`, `hero()` /
`hero_with()`, `meter_row()`, `count_row()`, `attention_row()`. Keeping them
in one module is what stops the capture and review screens from drifting
apart again.

### 9.4 Profile List (`components/profile_list.rs`)
- `ProfileListView`: Renders `PresenterModel.profiles` as rows
- Each row: name, enabled toggle, last-used timestamp
- Empty state: `AdwStatusPage` with "No profiles" message

### 9.5 PAM Services (`components/pam_services.rs`)
- Renders `PresenterModel.pam_services` as toggle rows
- Compatibility badges: Safe (green), Risky (orange), Unsupported (greyed out)
- High-impact confirmation dialog for risky enables

### 9.6 Recovery (`components/recovery.rs`)
- `RecoveryView`: Full-page error with recovery steps
- Used for daemon-unreachable, polkit-denied, service-not-registered

## 10. Evidence

### 10.1 Shell Tree (`docs/evidence/enrollment-redesign/09-shell-tree.txt`)
Text output enumerating destination titles and enrollment route.

### 10.2 Offline State (`docs/evidence/enrollment-redesign/09-shell-offline.txt`)
Text output showing offline state rendering (status page, retry action).