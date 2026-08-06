// SPDX-License-Identifier: GPL-3.0-only
//
// Nam-tor face profile KCM — QML/Kirigami UI (spec §9.3).
//
// Talks exclusively to org.namtor.Manager over D-Bus via the Rust KcmBridge
// (exposed to QML by the thin C++ KCModule shim). No biometric data, raw
// scores, or match thresholds are ever shown: the bridge surfaces only
// ProfileInfo, EnrollmentProgress (quality bucket + coverage %), and
// StatusInfo. Privileged actions (delete/replace/merge/enroll-other) are
// polkit-gated on the daemon side; the KCM only requests them.
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

    // NamtorKcmBridge exposed by the C++ shim; holds a KcmBridge connection.
    property var bridge: null

    // Bound from the bridge's ProfileList model.
    property var profiles: []

    // Bound from the bridge's StatusInfo.
    property bool daemonReachable: true
    property bool cameraPresent: false

    Component.onCompleted: {
        if (bridge) {
            bridge.refreshProfiles()
            bridge.refreshStatus()
        }
    }

    Kirigami.FormLayout {
        id: form

        Kirigami.Heading {
            Kirigami.FormData.isSection: true
            text: i18n("Face Authentication")
            level: 1
            Accessible.role: Accessible.Header
        }

        QQC2.Label {
            text: daemonReachable
                  ? (cameraPresent
                     ? i18n("Daemon reachable; camera available.")
                     : i18n("Daemon reachable; no camera detected."))
                  : i18n("Nam-tor daemon is not running. Start it to manage face profiles.")
            color: daemonReachable ? Kirigami.Theme.textColor : Kirigami.Theme.negativeTextColor
            wrapMode: Text.WordWrap
            Layout.fillWidth: true
            Accessible.role: Accessible.StaticText
            Accessible.name: text
        }

        Kirigami.Heading {
            Kirigami.FormData.isSection: true
            text: i18n("Profiles")
            level: 2
            Accessible.role: Accessible.Header
        }

        // Profile list as Kirigami cards. Each card is keyboard-focusable;
        // Tab/Enter activates the card's action row.
        Repeater {
            id: profileRepeater
            model: root.profiles

            Kirigami.Card {
                required property var modelData
                required property int index

                Layout.fillWidth: true
                focusPolicy: Qt.StrongFocus
                Accessible.role: Accessible.ListItem
                Accessible.name: i18n("Profile %1, %2, %3",
                                      modelData.name,
                                      modelData.enabled ? i18n("enabled") : i18n("disabled"),
                                      modelData.lastUsed
                                        ? i18n("last used %1").arg(modelData.lastUsed)
                                        : i18n("never used"))
                Accessible.description: i18n("Face profile. Activate to rename, merge, replace, enable, or delete.")

                contentItem: RowLayout {
                    spacing: Kirigami.Units.largeSpacing

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Kirigami.Title {
                            text: profileRepeater.itemAt(index).modelData.name
                            elide: Text.ElideRight
                        }
                        QQC2.Label {
                            text: profileRepeater.itemAt(index).modelData.enabled
                                  ? i18n("Enabled")
                                  : i18n("Disabled")
                            color: profileRepeater.itemAt(index).modelData.enabled
                                   ? Kirigami.Theme.positiveTextColor
                                   : Kirigami.Theme.disabledTextColor
                        }
                        QQC2.Label {
                            // Coverage/quality are shown only during enrollment (see wizard);
                            // here we show only the last-used timestamp, never a score.
                            text: profileRepeater.itemAt(index).modelData.lastUsed
                                  ? i18n("Last used: %1").arg(profileRepeater.itemAt(index).modelData.lastUsed)
                                  : i18n("Never used")
                            font: Kirigami.Theme.smallFont
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }

                    QQC2.ToolButton {
                        text: i18n("Rename")
                        icon.name: "edit-rename"
                        Accessible.name: i18n("Rename profile %1").arg(profileRepeater.itemAt(index).modelData.name)
                        onClicked: renameDialog.openFor(profileRepeater.itemAt(index).modelData.id,
                                                        profileRepeater.itemAt(index).modelData.name)
                    }
                    QQC2.ToolButton {
                        text: i18n("Replace")
                        icon.name: "view-refresh"
                        Accessible.name: i18n("Re-enroll profile %1").arg(profileRepeater.itemAt(index).modelData.name)
                        onClicked: replaceDialog.openFor(profileRepeater.itemAt(index).modelData.id,
                                                         profileRepeater.itemAt(index).modelData.name)
                    }
                    QQC2.ToolButton {
                        text: i18n("Delete")
                        icon.name: "edit-delete"
                        Accessible.name: i18n("Delete profile %1").arg(profileRepeater.itemAt(index).modelData.name)
                        // Polkit admin auth is requested by the daemon; we just call it.
                        onClicked: deleteDialog.openFor(profileRepeater.itemAt(index).modelData.id,
                                                        profileRepeater.itemAt(index).modelData.name)
                    }
                }
            }
        }

        QQC2.Button {
            text: i18n("Add Profile")
            icon.name: "list-add"
            enabled: root.daemonReachable
            Accessible.name: i18n("Enroll a new face profile")
            Accessible.description: i18n("Starts the enrollment wizard. You will be asked to look at the camera from several angles.")
            onClicked: enrollmentWizard.open()
        }
    }

    // ---- Enrollment wizard ----
    Kirigami.OverlaySheet {
        id: enrollmentWizard
        title: i18n("Enroll Face Profile")

        property string sessionId: ""

        function open() {
            // bridge.createProfile returns a session id; the wizard then drives
            // start -> poll progress -> finish/cancel.
            var sid = bridge.createProfile("")
            enrollmentWizard.sessionId = sid
            bridge.startEnrollment(sid)
            enrollmentWizard.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            QQC2.Label {
                text: i18n("Look at the camera. Move slowly through the guided poses.")
                wrapMode: Text.WordWrap
                Layout.fillWidth: true
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }

            // Progress: coverage percent and a quality bucket ("poor"/"fair"/
            // "good"/"excellent"). NEVER a numeric score or threshold.
            QQC2.ProgressBar {
                id: coverageBar
                Layout.fillWidth: true
                from: 0
                to: 100
                value: bridge.lastProgress ? bridge.lastProgress.coveragePercent : 0
                Accessible.role: Accessible.ProgressBar
                Accessible.name: i18n("Enrollment coverage: %1 percent").arg(value)
                Accessible.announcements: bridge.lastProgress
                    ? i18n("Coverage %1 percent, quality %2",
                           bridge.lastProgress.coveragePercent,
                           bridge.lastProgress.qualityLabel)
                    : ""
            }

            QQC2.Label {
                text: bridge.lastProgress
                      ? i18n("Quality: %1").arg(bridge.lastProgress.qualityLabel)
                      : i18n("Starting…")
                font: Kirigami.Theme.smallFont
                Accessible.role: Accessible.StaticText
                Accessible.name: text
            }

            RowLayout {
                spacing: Kirigami.Units.largeSpacing
                QQC2.Button {
                    text: i18n("Finish")
                    enabled: bridge.lastProgress && bridge.lastProgress.coveragePercent >= 60
                    Accessible.name: i18n("Finish enrollment")
                    onClicked: {
                        bridge.finishEnrollment(enrollmentWizard.sessionId)
                        enrollmentWizard.close()
                        bridge.refreshProfiles()
                    }
                }
                QQC2.Button {
                    text: i18n("Cancel")
                    Accessible.name: i18n("Cancel enrollment and discard captures")
                    onClicked: {
                        bridge.cancelEnrollment(enrollmentWizard.sessionId)
                        enrollmentWizard.close()
                    }
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
            renameDialog.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.TextField {
                id: renameField
                placeholderText: i18n("Profile name")
                Accessible.name: i18n("New profile name")
                onAccepted: renameConfirm.clicked()
            }
            QQC2.Button {
                id: renameConfirm
                text: i18n("Rename")
                enabled: renameField.text.length > 0
                onClicked: {
                    bridge.renameProfile(renameDialog.profileId, renameField.text)
                    renameDialog.close()
                    bridge.refreshProfiles()
                }
            }
        }
    }

    // ---- Replace (re-enroll) confirmation ----
    Kirigami.OverlaySheet {
        id: replaceDialog
        property string profileId: ""
        title: i18n("Replace Profile")

        function openFor(id, name) {
            replaceDialog.profileId = id
            replaceMessage.text = i18n("Replace the face profile for “%1” with a new enrollment? The old templates are discarded.").arg(name)
            replaceDialog.open()
        }

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing
            QQC2.Label { id: replaceMessage; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            RowLayout {
                QQC2.Button {
                    text: i18n("Replace")
                    icon.name: "view-refresh"
                    onClicked: {
                        // Re-enroll under a fresh name; the daemon discards old templates.
                        bridge.replaceProfile(replaceDialog.profileId, "")
                        replaceDialog.close()
                        enrollmentWizard.open()
                    }
                }
                QQC2.Button {
                    text: i18n("Cancel")
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
            QQC2.Label { id: deleteMessage; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            RowLayout {
                QQC2.Button {
                    text: i18n("Delete")
                    icon.name: "edit-delete"
                    Kirigami.Theme.colorSet: Kirigami.Theme.View
                    onClicked: {
                        bridge.deleteProfile(deleteDialog.profileId)
                        deleteDialog.close()
                        bridge.refreshProfiles()
                    }
                }
                QQC2.Button {
                    text: i18n("Cancel")
                    onClicked: deleteDialog.close()
                }
            }
        }
    }
}
