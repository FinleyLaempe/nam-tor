// SPDX-License-Identifier: GPL-3.0-only
//
// profile_model.h — QAbstractListModel for the Nam-tor profile list (spec §9.3).
//
// Exposes enrolled face profiles to QML as a list model. Carries only
// metadata (id, name, enabled, created, last_used) — no biometric data
// (embeddings, frames, scores, thresholds) per spec §9.4.

#ifndef NAMTOR_PROFILE_MODEL_H
#define NAMTOR_PROFILE_MODEL_H

#include <QAbstractListModel>
#include <QtQml/qqmlregistration.h>
#include <QVector>

struct NamtorProfileEntry {
    QString id;
    QString name;
    bool enabled;
    quint64 created;
    quint64 lastUsed;
};

class NamtorProfileModel : public QAbstractListModel {
    Q_OBJECT
    QML_ELEMENT

public:
    enum Roles {
        IdRole = Qt::UserRole + 1,
        NameRole,
        EnabledRole,
        CreatedRole,
        LastUsedRole,
    };
    Q_ENUM(Roles)

    explicit NamtorProfileModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role = Qt::DisplayRole) const override;
    QHash<int, QByteArray> roleNames() const override;

    void setProfiles(const QVector<NamtorProfileEntry> &profiles);
    const QVector<NamtorProfileEntry> &profiles() const;

private:
    QVector<NamtorProfileEntry> m_profiles;
};

#endif // NAMTOR_PROFILE_MODEL_H
