// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kcm.h — async D-Bus bridge QObject for the Nam-tor KCM (spec §9.3,
// Task 22).
//
// Exposes the frozen `org.namtor.Manager` contract (Task 8) to QML as typed
// QObject properties, Q_INVOKABLE methods, and Qt signals. All D-Bus calls
// are asynchronous via QDBusPendingCall + QDBusPendingCallWatcher so the GUI
// thread never blocks.
//
// Cancellation: each in-flight call is tracked by a generation counter. When
// the user cancels or restarts an operation, the generation is bumped; late
// responses from the old generation are silently dropped (stale-response
// sequencing).
//
// No biometric data (embeddings, frames, scores, thresholds) is exposed in
// any property or signal — only ProfileInfo metadata, EnrollmentProgress
// (quality bucket + coverage %), ReviewOutcome, and StatusReport.

#ifndef NAMTOR_KCM_H_CPP
#define NAMTOR_KCM_H_CPP

#include "namtor_kde.h"
#include "profile_model.h"

#include <QObject>
#include <QString>
#include <QtQml/qqmlregistration.h>

class QDBusInterface;
class QDBusPendingCallWatcher;

class NamtorKcm : public QObject {
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

public:
    explicit NamtorKcm(QObject *parent = nullptr);
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

    // --- Cancel all in-flight calls (stale-response sequencing) ---
    Q_INVOKABLE void cancelAllPending();

signals:
    // --- Frozen signal set (Task 8: 4 signals) ---
    void enrollmentProgress(const QString &session, quint32 framesCaptured,
                            const QString &qualityLabel, quint32 coveragePercent);
    void enrollmentCompleted(const QString &session, const QString &profile);
    void profileChanged(quint32 uid, const QString &id);
    void statusChanged();
    void loadingChanged();
    void errorChanged();

private:
    void setLoading(bool loading);
    void setError(const QString &error);
    void handleListProfilesReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleGetStatusReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleStartEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleCommitProfileReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleReviewProfileReply(QDBusPendingCallWatcher *watcher, quint64 gen);
    void handleVoidReply(QDBusPendingCallWatcher *watcher, quint64 gen);

    bool m_abiOk;
    QString m_abiError;
    NamtorProfileModel *m_profileModel;
    QDBusInterface *m_iface;

    bool m_daemonReachable;
    bool m_cameraPresent;
    bool m_emitterArmed;
    quint32 m_apiVersion;
    bool m_loading;
    QString m_error;

    quint64 m_generation;
    QVector<QDBusPendingCallWatcher *> m_pendingWatchers;
};

#endif // NAMTOR_KCM_H_CPP
