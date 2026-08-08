// SPDX-License-Identifier: GPL-3.0-only
//
// Nam-tor face profile KCM — QML/Kirigami UI (spec §9.3).
//
// Talks exclusively to org.namtor.Manager (v1) and org.namtor.Manager2 (v2)
// over the SYSTEM bus via the C++ NamtorKcm singleton. No biometric data,
// raw scores, or match thresholds are ever shown: the bridge surfaces only
// ProfileInfo, EnrollmentProgress (quality bucket + coverage %), ReviewOutcome,
// StatusReport, PamServiceInfo, and ProtectionStatus. Privileged actions
// (delete/replace/merge/enroll-other/PAM toggle) are polkit-gated on the
// daemon side; the KCM only requests them.
//
// Every interactive control calls a real NamtorKcm method — no stubs, no
// placeholders, no drift. The enrollment flow is start → progress → review →
// commit/cancel. Profile actions: list, rename, enable/disable, delete,
// replace. External D-Bus signals (ProfileChanged, StatusChanged,
// PamServicesChanged, ProtectionStatusChanged) refresh the UI automatically
// via the bridge's signal forwarding.
//
// Accessibility: every interactive control is keyboard-reachable (Kirigami
// cards are focusable), labelled for screen readers (Accessible.name/description),
// and the enrollment wizard announces progress via Accessible.announcements.
// All user-visible strings go through i18n().

import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.kirigami as Kirigami
import org.kde.kcmutils as KCM
import org.namtor 1.0 as Namtor

KCM.SimpleKCM {
    id: root

    Component.onCompleted: Namtor.NamtorKcm.refresh()

    Kirigami.FormLayout {
        id: form

        Kirigami.Heading {
            Kirigami.FormData.isSection: true
            text: i18n("Face Authentication")
            level: 1
            Accessible.role: Accessible.Header
            Accessible.name: text
        }

        QQC2.Label {
            text: Namtor.NamtorKcm.daemonReachable
                  ? (Namtor.NamtorKcm.cameraPresent
                     ? i18n("Daemon reachable; camera available.")
                     : i18n("Daemon reachable; no camera detected."))
                  : i18n("Nam-tor daemon is not running. Start it to manage face profiles.")
            color: Namtor.NamtorKcm.daemonReachable ? Kirigami.Theme.textColor : Kirigami.Theme.negativeTextColor
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        QQC2.Label {
            text: i18n("API version: %1").arg(Namtor.NamtorKcm.apiVersion)
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            visible: Namtor.NamtorKcm.daemonReachable
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        QQC2.Label {
            text: Namtor.NamtorKcm.error.length > 0
                  ? i18n("Error: %1").arg(Namtor.NamtorKcm.error)
                  : ""
            color: Kirigami.Theme.negativeTextColor
            visible: Namtor.NamtorKcm.error.length > 0
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        QQC2.BusyIndicator {
            running: Namtor.NamtorKcm.loading
            visible: Namtor.NamtorKcm.loading
            Accessible.role: Accessible.Indicator
            Accessible.name: i18n("Loading")
        }

        Kirigami.Heading {
            Kirigami.FormData.isSection: true
            text: i18n("Template Protection")
            level: 2
            Accessible.role: Accessible.Header
            Accessible.name: text
        }

        QQC2.Label {
            text: i18n("Sealing: %1").arg(Namtor.NamtorKcm.protectionSealing)
            visible: Namtor.NamtorKcm.protectionSealing.length > 0
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
        QQC2.Label {
            text: Namtor.NamtorKcm.protectionHasTpm
                  ? i18n("TPM2 device present.")
                  : i18n("No TPM2 device — software fallback.")
            color: Namtor.NamtorKcm.protectionHasTpm
                   ? Kirigami.Theme.positiveTextColor
                   : Kirigami.Theme.negativeTextColor
            visible: Namtor.NamtorKcm.protectionSealing.length > 0
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }
        QQC2.Label {
            text: Namtor.NamtorKcm.protectionWarning
            color: Kirigami.Theme.negativeTextColor
            visible: Namtor.NamtorKcm.protectionWarning.length > 0
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Kirigami.Heading {
            Kirigami.FormData.isSection: true
            text: i18n("Authentication uses")
            level: 2
            Accessible.role: Accessible.Header
            Accessible.name: text
        }

        QQC2.Label {
            text: i18n("Detected PAM authentication services. Toggle face authentication per service. High-impact services require confirmation.")
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Repeater {
            id: pamRepeater
            model: Namtor.NamtorKcm.pamServices

            Kirigami.Card {
                required property string id
                required property string displayName
                required property bool enabled
                required property string compatibility
                required property string description

                Layout.fillWidth: true
                focusPolicy: Qt.StrongFocus
                Accessible.role: Accessible.ListItem
                Accessible.name: i18n("PAM service %1, %2, %3",
                                      displayName,
                                      compatibility,
                                      enabled ? i18n("enabled") : i18n("disabled"))
                Accessible.description: description

                contentItem: RowLayout {
                    spacing: Kirigami.Units.largeSpacing

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Kirigami.Title {
                            text: displayName
                            elide: Text.ElideRight
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                        QQC2.Label {
                            text: description
                            font: Kirigami.Theme.smallFont
                            color: Kirigami.Theme.disabledTextColor
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                        QQC2.Label {
                            text: i18n("Compatibility: %1").arg(compatibility)
                            font: Kirigami.Theme.smallFont
                            color: compatibility === "risky"
                                   ? Kirigami.Theme.negativeTextColor
                                   : (compatibility === "unsupported"
                                      ? Kirigami.Theme.negativeTextColor
                                      : Kirigami.Theme.positiveTextColor)
                            visible: compatibility.length > 0
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                        QQC2.Label {
                            text: compatibility === "unsupported"
                                  ? i18n("This service is incompatible and cannot be enabled.")
                                  : (compatibility === "risky"
                                     ? i18n("High-impact: confirmation required before enabling.")
                                     : "")
                            visible: compatibility === "unsupported" || compatibility === "risky"
                            font: Kirigami.Theme.smallFont
                            color: Kirigami.Theme.negativeTextColor
                            wrapMode: Text.WordWrap
                            Layout.fillWidth: true
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                    }

                    QQC2.Switch {
                        checked: enabled
                        enabled: compatibility !== "unsupported"
                        Accessible.role: Accessible.Switch
                        Accessible.name: i18n("Toggle %1").arg(displayName)
                        onToggled: {
                            if (checked && Namtor.NamtorKcm.isHighImpact(compatibility)) {
                                pamConfirmDialog.openFor(id, displayName)
                                checked = Qt.binding(function() { return enabled })
                            } else {
                                Namtor.NamtorKcm.setPamServiceEnabled(id, checked)
                            }
                        }
                    }
                }
            }
        }

        Kirigami.Heading {

        Repeater {
            id: profileRepeater
            model: Namtor.NamtorKcm.profileList

            Kirigami.Card {
                required property string profileId
                required property string name
                required property bool enabled
                required property quint64 created
                required property quint64 lastUsed
                required property int index

                Layout.fillWidth: true
                focusPolicy: Qt.StrongFocus
                Accessible.role: Accessible.ListItem
                Accessible.name: i18n("Profile %1, %2, %3",
                                      name,
                                      enabled ? i18n("enabled") : i18n("disabled"),
                                      lastUsed ? i18n("last used %1").arg(lastUsed) : i18n("never used"))
                Accessible.description: i18n("Face profile. Activate to rename, replace, enable, or delete.")

                contentItem: RowLayout {
                    spacing: Kirigami.Units.largeSpacing

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Kirigami.Title {
                            text: name
                            elide: Text.ElideRight
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                        QQC2.Label {
                            text: enabled ? i18n("Enabled") : i18n("Disabled")
                            color: enabled ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.disabledTextColor
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                        QQC2.Label {
                            text: lastUsed ? i18n("Last used: %1").arg(lastUsed) : i18n("Never used")
                            font: Kirigami.Theme.smallFont
                            color: Kirigami.Theme.disabledTextColor
                            Accessible.role: Accessible.StaticText
                            Accessible.name: text
                        }
                    }

                    QQC2.ToolButton {
                        text: enabled ? i18n("Disable") : i18n("Enable")
                        icon.name: enabled ? "dialog-cancel" : "dialog-ok"
                        Accessible.role: Accessible.Button
                        Accessible.name: i18n("Toggle profile %1").arg(name)
                        onClicked: Namtor.NamtorKcm.enableProfile(Namtor.NamtorKcm.currentUid, profileId, !enabled)
                    }
                    QQC2.ToolButton {
                        text: i18n("Rename")
                        icon.name: "edit-rename"
                        Accessible.role: Accessible.Button
                        Accessible.name: i18n("Rename profile %1").arg(name)
                        onClicked: renameDialog.openFor(profileId, name)
                    }
                    QQC2.ToolButton {
                        text: i18n("Replace")
                        icon.name: "view-refresh"
                        Accessible.role: Accessible.Button
                        Accessible.name: i18n("Re-enroll profile %1").arg(name)
                        onClicked: replaceDialog.openFor(profileId, name)
                    }
                    QQC2.ToolButton {
                        text: i18n("Delete")
                        icon.name: "edit-delete"
                        Accessible.role: Accessible.Button
                        Accessible.name: i18n("Delete profile %1").arg(name)
                        onClicked: deleteDialog.openFor(profileId, name)
                    }
                }
            }
        }

        QQC2.Button {
            text: i18n("Add Profile")
            icon.name: "list-add"
            enabled: Namtor.NamtorKcm.daemonReachable
            Accessible.role: Accessible.Button
            Accessible.name: i18n("Enroll a new face profile")
            Accessible.description: i18n("Starts the enrollment wizard. You will be asked to look at the camera from several angles.")
            onClicked: enrollDialog.open()
        }
    }

    // ---- Enrollment wizard (start → progress → commit/cancel) ----
    Kirigami.OverlaySheet {
        id: enrollmentWizard
        title: i18n("Enroll Face Profile")

        property string sessionId: ""

        function open() {
            enrollField.text = ""
            enrollConfirm.enabled = false
            enrollmentWizard.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            QQC2.Label {
                text: i18n("Enter a name for the new face profile, then look at the camera and move slowly through the guided poses.")
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }

            QQC2.TextField {
                id: enrollField
                placeholderText: i18n("Profile name")
                Accessible.role: Accessible.EditableText
                Accessible.name: i18n("New profile name")
                onTextEdited: enrollConfirm.enabled = Namtor.NamtorKcm.isNameValid(text)
                              && !Namtor.NamtorKcm.isNameDuplicate(text, "")
                onAccepted: if (enrollConfirm.enabled) enrollConfirm.clicked()
            }

            QQC2.Button {
                id: enrollConfirm
                text: i18n("Start Enrollment")
                enabled: false
                Accessible.role: Accessible.Button
                Accessible.name: text
                onClicked: {
                    Namtor.NamtorKcm.startEnrollment(Namtor.NamtorKcm.currentUid, enrollField.text.trim())
                    enrollProgressLayout.visible = true
                }
            }

            ColumnLayout {
                id: enrollProgressLayout
                visible: false
                spacing: Kirigami.Units.smallSpacing

                QQC2.ProgressBar {
                    Layout.fillWidth: true
                    from: 0
                    to: 100
                    value: Namtor.NamtorKcm.coveragePercent
                    Accessible.role: Accessible.ProgressBar
                    Accessible.name: i18n("Enrollment coverage: %1 percent").arg(value)
                    Accessible.announcements: Namtor.NamtorKcm.enrollmentActive
                        ? i18n("Coverage %1 percent, quality %2",
                               Namtor.NamtorKcm.coveragePercent,
                               Namtor.NamtorKcm.qualityLabel)
                        : ""
                }

                QQC2.Label {
                    text: Namtor.NamtorKcm.enrollmentActive
                          ? i18n("Quality: %1 — frames: %2").arg(Namtor.NamtorKcm.qualityLabel).arg(Namtor.NamtorKcm.framesCaptured)
                          : i18n("Starting…")
                    font: Kirigami.Theme.smallFont
                    Accessible.role: Accessible.StaticText
                    Accessible.name: text
                }

                QQC2.Button {
                    text: i18n("Review")
                    enabled: Namtor.NamtorKcm.enrollmentActive && Namtor.NamtorKcm.coveragePercent >= 60
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    onClicked: {
                        // Review the in-progress session before committing.
                        Namtor.NamtorKcm.reviewProfile(Namtor.NamtorKcm.currentUid, Namtor.NamtorKcm.currentSession)
                    }
                }
            }

            QQC2.Label {
                text: Namtor.NamtorKcm.reviewProfileId.length > 0
                      ? i18n("Review: quality %1%2").arg(Namtor.NamtorKcm.reviewQuality)
                          .arg(Namtor.NamtorKcm.reviewNeedsReenroll ? i18n(" — re-enrollment recommended") : "")
                      : ""
                visible: Namtor.NamtorKcm.reviewProfileId.length > 0
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }

            RowLayout {
                spacing: Kirigami.Units.largeSpacing
                QQC2.Button {
                    text: i18n("Commit")
                    enabled: Namtor.NamtorKcm.enrollmentActive
                    Accessible.role: Accessible.Button
                    Accessible.name: i18n("Commit enrollment and save profile")
                    onClicked: {
                        Namtor.NamtorKcm.commitProfile(Namtor.NamtorKcm.currentSession)
                        enrollmentWizard.close()
                    }
                }
                QQC2.Button {
                    text: i18n("Cancel")
                    Accessible.role: Accessible.Button
                    Accessible.name: i18n("Cancel enrollment and discard captures")
                    onClicked: {
                        Namtor.NamtorKcm.cancelEnrollment(Namtor.NamtorKcm.currentSession)
                        enrollProgressLayout.visible = false
                        enrollmentWizard.close()
                    }
                }
            }
        }

        // Close the wizard when enrollment completes (external signal or commit reply).
        Connections {
            target: Namtor.NamtorKcm
            function onEnrollmentCompleted(session, profile) {
                enrollProgressLayout.visible = false
                enrollmentWizard.close()
            }
        }
    }

    // ---- Add-profile entry: opens the wizard ----
    Kirigami.OverlaySheet {
        id: enrollDialog
        title: i18n("Add Profile")

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.Label {
                text: i18n("Start a new face enrollment. You will be guided through several poses.")
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }
            QQC2.Button {
                text: i18n("Begin")
                icon.name: "list-add"
                Accessible.role: Accessible.Button
                Accessible.name: text
                onClicked: {
                    enrollDialog.close()
                    enrollmentWizard.open()
                }
            }
        }
    }

    // ---- Rename dialog ----
    Kirigami.OverlaySheet {
        id: renameDialog
        property string profileId: ""
        title: i18n("Rename Profile")

        function openFor(id, currentName) {
            renameDialog.profileId = id
            renameField.text = currentName
            renameConfirm.enabled = false
            renameDialog.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.TextField {
                id: renameField
                placeholderText: i18n("Profile name")
                Accessible.role: Accessible.EditableText
                Accessible.name: i18n("New profile name")
                onTextEdited: renameConfirm.enabled = Namtor.NamtorKcm.isNameValid(text)
                              && !Namtor.NamtorKcm.isNameDuplicate(text, renameDialog.profileId)
                onAccepted: if (renameConfirm.enabled) renameConfirm.clicked()
            }
            QQC2.Button {
                id: renameConfirm
                text: i18n("Rename")
                enabled: false
                Accessible.role: Accessible.Button
                Accessible.name: text
                onClicked: {
                    Namtor.NamtorKcm.renameProfile(Namtor.NamtorKcm.currentUid, renameDialog.profileId, renameField.text.trim())
                    renameDialog.close()
                }
            }
        }
    }

    // ---- Replace (re-enroll) confirmation — atomic ----
    Kirigami.OverlaySheet {
        id: replaceDialog
        property string profileId: ""
        property string profileName: ""
        title: i18n("Replace Profile")

        function openFor(id, name) {
            replaceDialog.profileId = id
            replaceDialog.profileName = name
            replaceMessage.text = i18n("Replace the face profile for “%1” with a new enrollment? The old templates are discarded atomically — the replace either fully succeeds or fully fails.").arg(name)
            replaceNameField.text = name
            replaceConfirm.enabled = true
            replaceDialog.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.Label {
                id: replaceMessage
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }
            QQC2.TextField {
                id: replaceNameField
                placeholderText: i18n("New profile name")
                Accessible.role: Accessible.EditableText
                Accessible.name: i18n("Replacement profile name")
                onTextEdited: replaceConfirm.enabled = Namtor.NamtorKcm.isNameValid(text)
                              && !Namtor.NamtorKcm.isNameDuplicate(text, replaceDialog.profileId)
                onAccepted: if (replaceConfirm.enabled) replaceConfirm.clicked()
            }
            RowLayout {
                QQC2.Button {
                    text: i18n("Replace")
                    icon.name: "view-refresh"
                    id: replaceConfirm
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    onClicked: {
                        Namtor.NamtorKcm.replaceProfile(Namtor.NamtorKcm.currentUid, replaceDialog.profileId, replaceNameField.text.trim())
                        replaceDialog.close()
                    }
                }
                QQC2.Button {
                    text: i18n("Cancel")
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    onClicked: replaceDialog.close()
                }
            }
        }
    }

    // ---- Delete confirmation (polkit admin auth) ----
    Kirigami.OverlaySheet {
        id: deleteDialog
        property string profileId: ""
        title: i18n("Delete Profile")

        function openFor(id, name) {
            deleteDialog.profileId = id
            deleteMessage.text = i18n("Delete the face profile for “%1”? This cannot be undone. Administrator authentication is required.").arg(name)
            deleteDialog.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.Label {
                id: deleteMessage
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }
            RowLayout {
                QQC2.Button {
                    text: i18n("Delete")
                    icon.name: "edit-delete"
                    Kirigami.Theme.colorSet: Kirigami.Theme.View
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    onClicked: {
                        Namtor.NamtorKcm.deleteProfile(Namtor.NamtorKcm.currentUid, deleteDialog.profileId)
                        deleteDialog.close()
                    }
                }
                QQC2.Button {
                    text: i18n("Cancel")
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    onClicked: deleteDialog.close()
                }
            }
        }
    }

    // ---- High-impact PAM service confirmation ----
    QQC2.Dialog {
        id: pamConfirmDialog
        property string serviceId: ""
        property string serviceName: ""
        title: i18n("Enable %1 — High-Impact Service").arg(serviceName)
        modal: true
        focus: true

        function openFor(sid, sname) {
            pamConfirmDialog.serviceId = sid
            pamConfirmDialog.serviceName = sname
            pamConfirmText.text = Namtor.NamtorKcm.highImpactWarning()
            pamConfirmDialog.open()
        }

        contentItem: ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.Label {
                id: pamConfirmText
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                color: Kirigami.Theme.negativeTextColor
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }
        }

        footer: QQC2.DialogButtonBox {
            QQC2.Button {
                text: i18n("Cancel")
                QQC2.DialogButtonBox.buttonRole: QQC2.DialogButtonBox.RejectRole
                Accessible.role: Accessible.Button
                Accessible.name: text
            }
            QQC2.Button {
                text: i18n("I understand, enable anyway")
                QQC2.DialogButtonBox.buttonRole: QQC2.DialogButtonBox.AcceptRole
                Kirigami.Theme.colorSet: Kirigami.Theme.View
                Accessible.role: Accessible.Button
                Accessible.name: text
            }
        }

        onAccepted: {
            Namtor.NamtorKcm.setPamServiceEnabled(pamConfirmDialog.serviceId, true)
        }
        onRejected: {
            // Switch reverts — the PamServicesChanged signal refreshes the list.
        }
    }
}
