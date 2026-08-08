// SPDX-License-Identifier: GPL-3.0-only
//
// profile_model.cpp — QAbstractListModel for the Nam-tor profile list.

#include "profile_model.h"

NamtorProfileModel::NamtorProfileModel(QObject *parent)
    : QAbstractListModel(parent)
{
}

int NamtorProfileModel::rowCount(const QModelIndex &parent) const
{
    if (parent.isValid())
        return 0;
    return m_profiles.size();
}

QVariant NamtorProfileModel::data(const QModelIndex &index, int role) const
{
    if (!index.isValid() || index.row() < 0 || index.row() >= m_profiles.size())
        return {};
    const auto &p = m_profiles.at(index.row());
    switch (role) {
    case IdRole:      return p.id;
    case NameRole:    return p.name;
    case EnabledRole: return p.enabled;
    case CreatedRole: return p.created;
    case LastUsedRole: return p.lastUsed;
    }
    return {};
}

QHash<int, QByteArray> NamtorProfileModel::roleNames() const
{
    return {
        {IdRole,      "profileId"},
        {NameRole,    "name"},
        {EnabledRole, "enabled"},
        {CreatedRole, "created"},
        {LastUsedRole,"lastUsed"},
    };
}

void NamtorProfileModel::setProfiles(const QVector<NamtorProfileEntry> &profiles)
{
    beginResetModel();
    m_profiles = profiles;
    endResetModel();
}

const QVector<NamtorProfileEntry> &NamtorProfileModel::profiles() const
{
    return m_profiles;
}
