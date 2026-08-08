# Nam-tor GNOME Design System

This document defines the visual and interaction language for the Nam-tor GNOME application. The UI follows the GNOME Human Interface Guidelines (HIG) and leverages `libadwaita` to provide a native, accessible, and polished experience.

## 1. Design Philosophy
- **Native & Seamless**: Use Adwaita components strictly. Avoid custom "brand" styles that clash with the GNOME desktop environment.
- **Atomic Interaction**: Every operation (enrollment, deletion, etc.) must have a clear start, progress, and resolution state.
- **Safe by Default**: Biometric data is never surfaced. Feedback is coarse (e.g., "High Quality") to maintain privacy.
- **Resilient**: The UI must remain responsive even if the `org.namtor.Manager` daemon is slow or unavailable.

## 2. Visual Tokens

### 2.1 Typography (Adwaita Scale)
| Role | Adwaita Class | Purpose |
|---|---|---|
| Header | `.title` | Main page titles, Section headers |
| Body | (default) | Profile names, status messages |
| Caption | `.caption` | Timestamps, metadata, daemon version |
| Dimmed | `.dim-label` | Secondary information, inactive states |
| Error | `.error` | Critical failures, daemon unavailable |
| Warning | `.warning` | Low quality, needs re-enrollment |

### 2.2 Spacing & Layout
- **Base Grid**: 8px / 4px.
- **Margins**: 12px for main content windows; 24px for centered recovery pages.
- **Gaps**: 4px for related elements (e.g., within a profile row), 8px for layout blocks.
- **Alignment**: All labels are `halign(Align::Start)` by default.

### 2.3 Components

#### Profile Row
- **Structure**: `[Name (expands)] [Enabled Toggle] [Last Used (dimmed)]`
- **Interaction**: Row is an `AccessibleRole::ListItem`. Toggle controls the active status.

#### Enrollment Card
- **Container**: `adw::Frame` with label "Enrollment".
- **Content**: 
  - Text: "Frames: {n} · Coverage: {x}% · Quality: {y}"
  - Progress: `gtk4::ProgressBar` showing coverage.
  - Guidance: Conditional `.warning` label for low diversity.

#### Status Bar (Top)
- **Content**: "Daemon {up/down} · Camera {present/absent} · v{version}"
- **Style**: `.dim-label .caption`

#### Error / Recovery Page
- **Layout**: Centered `GtkBox` (Vertical).
- **Structure**: Title (Error) → Detail (Symptom) → Recovery Steps (Bullet list).
- **Action**: Clear "Recovery" section providing specific shell commands.

## 3. State Machine & Transitions

| State | Trigger | Visual Representation |
|---|---|---|
| **Loading** | App Start | Status bar shows "Connecting...", main list empty. |
| **Ready** | D-Bus connected | Profile list rendered; status bar "up". |
| **Unreachable**| D-Bus timeout/error | Full-page Recovery view (No list, no toolbar). |
| **Enrolling** | Start clicked | Enrollment card appears below profile list. |
| **Error** | Operation failed | `.error` label appears at bottom of view. |

## 4. Accessibility (GTK4)
- Every control must have a meaningful `accessible_role` (e.g., `ListItem`, `Checkbox`, `Alert`).
- Navigation must be fully keyboard-operable via Tab/Enter.
- Error messages must use the `Alert` role to be picked up by screen readers.
