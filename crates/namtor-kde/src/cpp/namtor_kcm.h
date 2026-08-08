// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kcm.h — thin C++ KCModule shim that exposes the Rust bridge's C
// ABI to QML (spec §9.3). The full async D-Bus bridge — method calls, signal
// forwarding, the QML profile model — is Task 22 and is NOT implemented here.
//
// What this file does (Task 21, build scaffolding only):
//   1. Loads the Rust cdylib and checks `namtor_kde_abi_version()` against the
//      compile-time `NAMTOR_KDE_ABI_VERSION`. On mismatch the shim is
//      fail-closed: `abiOk` is false and `abiError` carries a message, so QML
//      renders the offline/error state instead of loading an incompatible
//      bridge.
//   2. Surfaces the canonical `org.namtor.Manager` D-Bus bus name and object
//      path as Q_PROPERTY strings so QML (or a Task 22 Qt D-Bus proxy) does
//      not hardcode them a second time.
//
// No biometric data is handled here. The QObject carries only the ABI check
// result and two public D-Bus address strings.

#ifndef NAMTOR_KCM_H_CPP
#define NAMTOR_KCM_H_CPP

#include "namtor_kde.h"

#include <QObject>
#include <QString>
#include <QtQml/qqmlregistration.h>

class NamtorKcm : public QObject {
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

    Q_PROPERTY(QString managerBusName READ managerBusName CONSTANT)
    Q_PROPERTY(QString managerObjectPath READ managerObjectPath CONSTANT)
    Q_PROPERTY(bool abiOk READ abiOk CONSTANT)
    Q_PROPERTY(QString abiError READ abiError CONSTANT)

public:
    explicit NamtorKcm(QObject *parent = nullptr);

    QString managerBusName() const;
    QString managerObjectPath() const;
    bool abiOk() const;
    QString abiError() const;

private:
    bool m_abiOk;
    QString m_abiError;
};

#endif // NAMTOR_KCM_H_CPP
