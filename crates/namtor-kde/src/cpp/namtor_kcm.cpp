// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kcm.cpp — implementation of the NamtorKcm QML singleton (see
// namtor_kcm.h for the build-scaffolding contract). The full async D-Bus
// bridge is Task 22.

#include "namtor_kcm.h"

NamtorKcm::NamtorKcm(QObject *parent)
    : QObject(parent)
    , m_abiOk(false)
{
    const uint32_t got = namtor_kde_abi_version();
    if (got == NAMTOR_KDE_ABI_VERSION) {
        m_abiOk = true;
    } else {
        m_abiOk = false;
        m_abiError = QStringLiteral(
            "Nam-tor KCM ABI mismatch: header=%1 cdylib=%2. "
            "Reinstall the namtor-kde package."
        ).arg(NAMTOR_KDE_ABI_VERSION).arg(got);
    }
}

QString NamtorKcm::managerBusName() const
{
    return QString::fromUtf8(namtor_kde_manager_bus_name());
}

QString NamtorKcm::managerObjectPath() const
{
    return QString::fromUtf8(namtor_kde_manager_object_path());
}

bool NamtorKcm::abiOk() const { return m_abiOk; }
QString NamtorKcm::abiError() const { return m_abiError; }
