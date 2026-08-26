// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kcm.h — async D-Bus bridge QObject for the Nam-tor KCM (spec §9.3,
// Tasks 22-24).
//
// Exposes the frozen `org.namtor.Manager` (v1, Task 8), `org.namtor.Manager2`
// (v2, Task 1), and `org.namtor.Manager3` (v3, enrollment redesign) contracts
// to QML as typed QObject properties, Q_INVOKABLE methods, and Qt signals.
// All D-Bus calls are asynchronous via QDBusPendingCall +
// QDBusPendingCallWatcher so the GUI thread never blocks.
//
// Bus: ALL calls go through QDBusConnection::systemBus()—the daemon is a
// system service. The KCM never used the session bus.
//
// Cancellation: each in-flight call is tracked by a generation counter. When
// the user cancels or restarts an operation, the generation is bumped; late
// responses from the old generation are silently dropped (stale-response
// sequencing).
//
// D-Bus signal forwarding: the bridge connects to the four frozen v1 Manager
// signals (EnrollmentProgress, EnrollmentCompleted, ProfileChanged,
// StatusChanged), the two v2 Manager2 signals (PamServicesChanged,
// ProtectionStatusChanged), and the four v3 Manager3 signals
// (EnrollmentProgress with the expanded (ssuuuuuys) struct,
// EnrollmentCompleted, ProfileChanged, StatusChanged). v1/v2 signals are
// re-emitted as Qt signals unchanged. The v3 EnrollmentProgress signal
// updates the cached v3 progress properties (phase, qualityPercent,
// diversityPercent, framesSeen, framesAccepted, framesRejected,
// qualityBucket, guidance) and re-emits as enrollmentProgressV3 so QML can
// bind without inspecting signal arguments. ProfileChanged triggers an
// automatic listProfiles refresh so external mutations are reflected.
//
// v3 enrollment lifecycle: the v3 surface adds Stop/Resume/Review between
// Start and Commit. QML drives it as: startEnrollmentV3 → (progress) →
// stopEnrollment → reviewEnrollment → commitProfileV3 (save) or
// cancelEnrollmentV3 (discard), with resumeEnrollment to return to capturing
// after a stop. The v1 startEnrollment/cancelEnrollment/commitProfile
// methods are preserved unchanged for v1 clients.
//
// No biometric data (embeddings, frames, scores, thresholds) is exposed in
// any property or signal—only ProfileInfo metadata, EnrollmentProgress
// (quality bucket + coverage %), ReviewOutcome, StatusReport, PamServiceInfo,
// ProtectionStatus, and the v3 progress/review structs (coarse percents,
// counters, a quality bucket byte, and guidance text).

#ifndef NAMTOR_KCM_H_CPP
#define NAMTOR_KCM_H_CPP

#include "namtor_kde.h"
#include "profile_model.h"

#include <QObject>
#include <QString>
#include <QtQml/qqmlregistration.h>
#include <QtDBus/QDBusMessage>

#ifdef NAMTOR_KDE_WITH_KCM
#include <KQuickConfigModule>
#include <KPluginMetaData>
using NamtorKcmBase = KQuickConfigModule;
#else
using NamtorKcmBase = QObject;
#endif

class QDBusInterface;
class QDBusPendingCallWatcher;

class NamtorKcm : public NamtorKcmBase {
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

    // --- ABI check (from Task 21 scaffolding) ---
    Q_PROPERTY(QString managerBusName READ managerBusName CONSTANT)
    Q_PROPERTY(QString managerObjectPath READ managerObjectPath CONSTANT)
    Q_PROPERTY(bool abiOk READ abiOk CONSTANT)
    Q_PROPERTY(QString abiError READ abiError CONSTANT)

    // --- Typed QObject properties ---
    Q_PROPERTY(NamtorProfileModel *profileList READ profileList CONSTANT)
    Q_PROPERTY(bool daemonReachable READ daemonReachable NOTIFY statusChanged)
    Q_PROPERTY(bool cameraPresent READ cameraPresent NOTIFY statusChanged)
    Q_PROPERTY(bool emitterArmed READ emitterArmed NOTIFY statusChanged)
    Q_PROPERTY(bool loading READ loading NOTIFY loadingChanged)
    Q_PROPERTY(QString error READ error NOTIFY errorChanged)
    Q_PROPERTY(quint32 apiVersion READ apiVersion NOTIFY statusChanged)

    // --- Enrollment state (Task 23) ---
    Q_PROPERTY(quint32 currentUid READ currentUid CONSTANT)
    Q_PROPERTY(QString currentSession READ currentSession NOTIFY currentSessionChanged)
    Q_PROPERTY(bool enrollmentActive READ enrollmentActive NOTIFY enrollmentActiveChanged)
    Q_PROPERTY(quint32 framesCaptured READ framesCaptured NOTIFY enrollmentProgress)
    Q_PROPERTY(QString qualityLabel READ qualityLabel NOTIFY enrollmentProgress)
    Q_PROPERTY(quint32 coveragePercent READ coveragePercent NOTIFY enrollmentProgress)

    // --- Review state (Task 23) ---
    Q_PROPERTY(QString reviewProfileId READ reviewProfileId NOTIFY reviewReady)
    Q_PROPERTY(bool reviewNeedsReenroll READ reviewNeedsReenroll NOTIFY reviewReady)
    Q_PROPERTY(QString reviewQuality READ reviewQuality NOTIFY reviewReady)

    // --- v3 enrollment state (org.namtor.Manager3) ---
    Q_PROPERTY(QString enrollmentPhase READ enrollmentPhase NOTIFY enrollmentProgressV3)
    Q_PROPERTY(quint32 qualityPercent READ qualityPercent NOTIFY enrollmentProgressV3)
    Q_PROPERTY(quint32 diversityPercent READ diversityPercent NOTIFY enrollmentProgressV3)
    Q_PROPERTY(quint32 framesSeen READ framesSeen NOTIFY enrollmentProgressV3)
    Q_PROPERTY(quint32 framesAccepted READ framesAccepted NOTIFY enrollmentProgressV3)
    Q_PROPERTY(quint32 framesRejected READ framesRejected NOTIFY enrollmentProgressV3)
    Q_PROPERTY(quint8 qualityBucket READ qualityBucket NOTIFY enrollmentProgressV3)
    Q_PROPERTY(QString guidance READ guidance NOTIFY enrollmentProgressV3)

    // --- v3 review state (org.namtor.Manager3) ---
    Q_PROPERTY(bool reviewReadyToSave READ reviewReadyToSave NOTIFY reviewReadyV3)
    Q_PROPERTY(quint32 reviewQualityPercent READ reviewQualityPercent NOTIFY reviewReadyV3)
    Q_PROPERTY(quint32 reviewDiversityPercent READ reviewDiversityPercent NOTIFY reviewReadyV3)
    Q_PROPERTY(quint32 reviewFramesAccepted READ reviewFramesAccepted NOTIFY reviewReadyV3)
    Q_PROPERTY(QString reviewGuidance READ reviewGuidance NOTIFY reviewReadyV3)

    // --- v2 ProtectionStatus (org.namtor.Manager2) ---
    Q_PROPERTY(QString protectionSealing READ protectionSealing NOTIFY protectionStatusChanged)
    Q_PROPERTY(bool protectionHasTpm READ protectionHasTpm NOTIFY protectionStatusChanged)
    Q_PROPERTY(QString protectionWarning READ protectionWarning NOTIFY protectionStatusChanged)

    // --- v2 PAM services (org.namtor.Manager2) ---
    Q_PROPERTY(QVariantList pamServices READ pamServices NOTIFY pamServicesChanged)

public:
    explicit NamtorKcm(QObject *parent = nullptr);
#ifdef NAMTOR_KDE_WITH_KCM
    explicit NamtorKcm(QObject *parent, const KPluginMetaData &metaData);
    QUrl sourceUrl() const override { return QUrl(QStringLiteral("qrc:/qt/qml/org/namtor/main.qml")); }
#endif
    ~NamtorKcm() override;

    // ABI properties
    QString managerBusName() const;
    QString managerObjectPath() const;
    bool abiOk() const;
    QString abiError() const;

    // Typed properties
    NamtorProfileModel *profileList() const;
    bool daemonReachable() const;
    bool cameraPresent() const;
    bool emitterArmed() const;
    bool loading() const;
    QString error() const;
    quint32 apiVersion() const;

    // Enrollment state properties
    quint32 currentUid() const;
    QString currentSession() const;
    bool enrollmentActive() const;
    quint32 framesCaptured() const;
    QString qualityLabel() const;
    quint32 coveragePercent() const;

    // Review state properties
    QString reviewProfileId() const;
    bool reviewNeedsReenroll() const;
    QString reviewQuality() const;

    // v3 enrollment state properties
    QString enrollmentPhase() const;
    quint32 qualityPercent() const;
    quint32 diversityPercent() const;
    quint32 framesSeen() const;
    quint32 framesAccepted() const;
    quint32 framesRejected() const;
    quint8 qualityBucket() const;
    QString guidance() const;

    // v3 review state properties
    bool reviewReadyToSave() const;
    quint32 reviewQualityPercent() const;
    quint32 reviewDiversityPercent() const;
    quint32 reviewFramesAccepted() const;
    QString reviewGuidance() const;

    // v2 ProtectionStatus properties
    QString protectionSealing() const;
    bool protectionHasTpm() const;
    QString protectionWarning() const;

    // v2 PAM services property
    QVariantList pamServices() const;

    // --- Q_INVOKABLE methods (frozen D-Bus contract, Task 8) ---
    Q_INVOKABLE void listProfiles();
    Q_INVOKABLE void startEnrollment(quint32 uid, const QString &name);
    Q_INVOKABLE void cancelEnrollment(const QString &session);
    Q_INVOKABLE void commitProfile(const QString &session);
    Q_INVOKABLE void reviewProfile(quint32 uid, const QString &id);
    Q_INVOKABLE void renameProfile(quint32 uid, const QString &id, const QString &name);
    Q_INVOKABLE void enableProfile(quint32 uid, const QString &id, bool enabled);
    Q_INVOKABLE void deleteProfile(quint32 uid, const QString &id);
    Q_INVOKABLE void mergeProfiles(quint32 uid, const QString &src, const QString &dst);
    Q_INVOKABLE void replaceProfile(quint32 uid, const QString &id, const QString &name);
    Q_INVOKABLE void getStatus();

    // --- Q_INVOKABLE methods (v2 org.namtor.Manager2 contract, Task 1) ---
    Q_INVOKABLE void listPamServices();
    Q_INVOKABLE void setPamServiceEnabled(const QString &serviceId, bool enabled);
    Q_INVOKABLE void getProtectionStatus();

    // --- Q_INVOKABLE methods (v3 org.namtor.Manager3 continuous enrollment) ---
    Q_INVOKABLE void startEnrollmentV3(quint32 uid, const QString &name);
    Q_INVOKABLE void stopEnrollment(const QString &session);
    Q_INVOKABLE void resumeEnrollment(const QString &session);
    Q_INVOKABLE void reviewEnrollment(const QString &session);
    Q_INVOKABLE void commitProfileV3(const QString &session);
    Q_INVOKABLE void cancelEnrollmentV3(const QString &session);

    // --- Refresh all state (convenience for Component.onCompleted) ---
    Q_INVOKABLE void refresh();

    // --- Cancel all in-flight calls (stale-response sequencing) ---
    Q_INVOKABLE void cancelAllPending();

    // --- Validation helpers for QML ---
    Q_INVOKABLE bool isNameValid(const QString &name) const;
    Q_INVOKABLE bool isNameDuplicate(const QString &name, const QString &excludeId) const;

    // --- Profile lookup by id (removes QML index coupling) ---
    Q_INVOKABLE QString profileNameById(const QString &id) const;

    // --- v2 PAM high-impact helpers for QML ---
    Q_INVOKABLE QString highImpactWarning() const;
    Q_INVOKABLE bool isHighImpact(const QString &compatibility) const;

signals:
    // --- Frozen signal set (Task 8: 4 signals) ---
    void enrollmentProgress(const QString &session, quint32 framesCaptured,
                            const QString &qualityLabel, quint32 coveragePercent);
    void enrollmentCompleted(const QString &session, const QString &profile);
    void profileChanged(quint32 uid, const QString &id);
    void statusChanged();
    void loadingChanged();
    void errorChanged();

    // --- Enrollment lifecycle (Task 23) ---
    void enrollmentStarted(const QString &session);
    void currentSessionChanged();
    void enrollmentActiveChanged();

    // --- Review outcome (Task 23) ---
    void reviewReady(const QString &profileId, bool needsReenroll, const QString &quality);

    // --- v2 signals (org.namtor.Manager2) ---
    void pamServicesChanged();
    void protectionStatusChanged();

    // --- v3 signals (org.namtor.Manager3) ---
    void enrollmentProgressV3(const QString &session, const QString &phase,
                              quint32 qualityPercent, quint32 diversityPercent,
                              quint32 framesSeen, quint32 framesAccepted,
                              quint32 framesRejected, quint8 qualityBucket,
                              const QString &guidance);
    void reviewReadyV3(const QString &session, bool readyToSave,
                       quint32 qualityPercent, quint32 diversityPercent,
                       quint32 framesAccepted, const QString &guidance);
    void enrollmentStopped(const QString &session);
    void enrollmentResumed(const QString &session);

private:
    void setLoading(bool loading);
    void setError(const QString &error);
    void setCurrentSession(const QString &session);
    void setEnrollmentActive(bool active);
    void handleListProfilesReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleGetStatusReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleStartEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleCommitProfileReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleReviewProfileReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleVoidReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleListPamServicesReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleSetPamServiceEnabledReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleGetProtectionStatusReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleStartEnrollmentV3Reply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleStopEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleResumeEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleReviewEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleCommitProfileV3Reply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleCancelEnrollmentV3Reply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void connectDBusSignals();

private slots:
    void onEnrollmentProgressDBus(const QDBusMessage &msg);
    void onEnrollmentCompletedDBus(const QDBusMessage &msg);
    void onProfileChangedDBus(const QDBusMessage &msg);
    void onStatusChangedDBus(const QDBusMessage &msg);
    void onPamServicesChangedDBus(const QDBusMessage &msg);
    void onProtectionStatusChangedDBus(const QDBusMessage &msg);
    void onEnrollmentProgressV3DBus(const QDBusMessage &msg);

private:
    bool m_abiOk;
    QString m_abiError;
    NamtorProfileModel *m_profileModel;
    QDBusInterface *m_iface;
    QDBusInterface *m_iface2;
    QDBusInterface *m_iface3;

    bool m_daemonReachable;
    bool m_cameraPresent;
    bool m_emitterArmed;
    quint32 m_apiVersion;
    bool m_loading;
    QString m_error;

    // Enrollment state
    quint32 m_currentUid;
    QString m_currentSession;
    bool m_enrollmentActive;
    quint32 m_framesCaptured;
    QString m_qualityLabel;
    quint32 m_coveragePercent;

    // Review state
    QString m_reviewProfileId;
    bool m_reviewNeedsReenroll;
    QString m_reviewQuality;

    // v2 ProtectionStatus
    QString m_protectionSealing;
    bool m_protectionHasTpm;
    QString m_protectionWarning;

    // v2 PAM services (list of QVariantMap per PamServiceInfo)
    QVariantList m_pamServices;

    // v3 enrollment state
    QString m_enrollmentPhase;
    quint32 m_qualityPercent;
    quint32 m_diversityPercent;
    quint32 m_framesSeen;
    quint32 m_framesAccepted;
    quint32 m_framesRejected;
    quint8 m_qualityBucket;
    QString m_guidance;

    // v3 review state
    bool m_reviewReadyToSave;
    quint32 m_reviewQualityPercent;
    quint32 m_reviewDiversityPercent;
    quint32 m_reviewFramesAccepted;
    QString m_reviewGuidance;

    quint64 m_generation;
    QVector<QDBusPendingCallWatcher *> m_pendingWatchers;
};

#endif // NAMTOR_KCM_H_CPP
