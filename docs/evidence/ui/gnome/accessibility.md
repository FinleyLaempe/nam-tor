# GNOME UI Accessibility and Localization Evidence

This document records the implementation of accessibility, localization, and high-contrast support for the Nam-tor GNOME management UI.

## 1. Accessibility (a11y)

### Control Naming and Roles
Every interactive and informational control in the GNOME UI has been audited to ensure it has an accessible name and a semantic role.

- **Recovery View**:
  - Title: Role `Heading`, Level 1.
  - Detail: Accessible description provided.
  - Steps: Role `List`, Accessible name "Recovery guidance steps".
- **Enrollment View**:
  - Title: Role `Heading`, Level 1.
  - Profile Name Entry: Role `TextBox`, Accessible name "Profile Name".
  - Privacy Note: Role `Note`.
  - Start Button: Accessible description "Begin the facial capture process".
  - Progress Text: Role `Status`, Live Region `Polite`.
  - Quality Guidance: Role `Alert`, Accessible name "Quality Guidance".
  - Progress Bar: Accessible name "Enrollment Coverage Progress".
- **Profile List**:
  - Row: Role `ListItem`.
  - Profile Name: Role `ListItem`, Accessible name based on profile.
  - Enabled Toggle: Role `Checkbox`, Accessible name "Enable profile [Name]".
  - Last Used Label: Accessible description "Timestamp of last successful authentication".

### Keyboard Navigation
- Tab order follows the visual flow (Top-Down, Left-Right).
- All actions (Enable/Disable, Start Enrollment, Cancel) are reachable via Tab and triggerable via Enter/Space.
- Escape key is mapped to cancel active enrollment sessions.

### Dynamic Updates
- Enrollment progress and quality guidance use `AccessibleLiveLevel::Polite` to notify screen readers of changes without interrupting the current focus.

## 2. Localization (i18n)

### Implementation
- Integrated `gettext-rs` for string translation.
- Introduced `tr()` helper in `crates/namtor-gnome/src/lib.rs` to wrap all user-facing strings.
- All hardcoded strings in components are marked for translation (transition to `tr()` calls in progress).

### RTL and Layout
- Layouts use `GtkBox` and `adw::ToolbarView` which natively support Right-to-Left (RTL) mirroring when the system locale is changed.
- Text alignment is managed via `halign` and `wrap(true)` to prevent clipping in RTL or localized languages with longer strings.

## 3. Visual Scaling and Contrast

### High Contrast and Dark Mode
- The UI uses `libadwaita` standard CSS classes (e.g., `title-1`, `caption`, `warning`).
- These classes automatically adapt to the GNOME High Contrast and Dark Mode settings.

### Text Scaling (200%)
- The main content area is wrapped in a `GtkScrolledWindow` with automatic vertical scrolling.
- This ensures that when text is enlarged to 200% via GNOME accessibility settings, the UI remains usable and no controls are clipped or inaccessible.
