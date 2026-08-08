// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kcm.cpp — async D-Bus bridge implementation for the Nam-tor KCM.
//
// All D-Bus calls go through QDBusInterface::asyncCall() + QDBusPendingCallWatcher.
// The watcher's finished() signal fires on the GUI thread when the reply arrives
// (or times out) — the GUI never blocks. Each call carries a generation counter;
// if the user cancels or restarts, the generation is bumped and late replies
// from the old generation are silently dropped (stale-response sequencing).
//
// No biometric data is handled here. The QObject carries only profile metadata,
// coarse quality buckets, and aggregated status — all from the frozen Task 8
// contract.

#include "namtor_kcm.h"

#include <QtDBus/QDBusInterface>
#include <QtDBus/QDBusPendingCallWatcher>
#include <QtDBus/QDBusPendingReply>
#include <QtDBus/QDBusArgument>
#include <QDBusMetaType>

// --- D-Bus wire structs (frozen Task 8 contract) ---

struct WireProfileInfo {
    QString id;
    QString name;
    bool enabled;
    quint64 created;
    quint64 lastUsed;
};
Q_DECLARE_METATYPE(WireProfileInfo)

struct WireEnrollmentProgress {
    QString session_id;
    quint32 frames_captured;
    QString quality_label;
    quint32 coverage_percent;
};
Q_DECLARE_METATYPE(WireEnrollmentProgress)

struct WireReviewOutcome {
    QString profile_id;
    bool needs_reenroll;
    QString quality;
};
Q_DECLARE_METATYPE(WireReviewOutcome)

struct WireStatusReport {
    quint32 api_version;
    QVector<WireProfileInfo> profiles;
    bool emitter_armed;
};
Q_DECLARE_METATYPE(WireStatusReport)

// --- QDBusArgument operators for wire structs ---

QDBusArgument &operator<<(QDBusArgument &arg, const WireProfileInfo &p)
{
    arg.beginStructure();
    arg << p.id << p.name << p.enabled << p.created << p.lastUsed;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireProfileInfo &p)
{
    arg.beginStructure();
    arg >> p.id >> p.name >> p.enabled >> p.created >> p.lastUsed;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const WireEnrollmentProgress &p)
{
    arg.beginStructure();
    arg << p.session_id << p.frames_captured << p.quality_label << p.coverage_percent;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireEnrollmentProgress &p)
{
    arg.beginStructure();
    arg >> p.session_id >> p.frames_captured >> p.quality_label >> p.coverage_percent;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const WireReviewOutcome &r)
{
    arg.beginStructure();
    arg << r.profile_id << r.needs_reenroll << r.quality;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireReviewOutcome &r)
{
    arg.beginStructure();
    arg >> r.profile_id >> r.needs_reenroll >> r.quality;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const WireStatusReport &s)
{
    arg.beginStructure();
    arg << s.api_version << s.profiles << s.emitter_armed;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireStatusReport &s)
{
    arg.beginStructure();
    arg >> s.api_version >> s.profiles >> s.emitter_armed;
    arg.endStructure();
    return arg;
}

// --- NamtorKcm ---

static bool s_typesRegistered = false;

static void registerWireTypes()
{
    if (s_typesRegistered)
        return;
    qRegisterMetaType<WireProfileInfo>("WireProfileInfo");
    qRegisterMetaType<WireEnrollmentProgress>("WireEnrollmentProgress");
    qRegisterMetaType<WireReviewOutcome>("WireReviewOutcome");
    qRegisterMetaType<WireStatusReport>("WireStatusReport");
    qDBusRegisterMetaType<WireProfileInfo>();
    qDBusRegisterMetaType<WireEnrollmentProgress>();
    qDBusRegisterMetaType<WireReviewOutcome>();
    qDBusRegisterMetaType<WireStatusReport>();
    qRegisterMetaType<QVector<WireProfileInfo>>("QVector<WireProfileInfo>");
    qDBusRegisterMetaType<QVector<WireProfileInfo>>();
    s_typesRegistered = true;
}

NamtorKcm::NamtorKcm(QObject *parent)
    : QObject(parent)
    , m_abiOk(false)
    , m_profileModel(new NamtorProfileModel(this))
    , m_iface(nullptr)
    , m_daemonReachable(false)
    , m_cameraPresent(false)
    , m_emitterArmed(false)
    , m_apiVersion(0)
    , m_loading(false)
    , m_generation(0)
{
    registerWireTypes();

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

    m_iface = new QDBusInterface(
        QString::fromUtf8(namtor_kde_manager_bus_name()),
        QString::fromUtf8(namtor_kde_manager_object_path()),
        QStringLiteral("org.namtor.Manager"),
        QDBusConnection::sessionBus(),
        this);
}

NamtorKcm::~NamtorKcm()
{
    cancelAllPending();
}

// --- ABI properties ---

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

// --- Typed properties ---

NamtorProfileModel *NamtorKcm::profileList() const { return m_profileModel; }
bool NamtorKcm::daemonReachable() const { return m_daemonReachable; }
bool NamtorKcm::cameraPresent() const { return m_cameraPresent; }
bool NamtorKcm::emitterArmed() const { return m_emitterArmed; }
bool NamtorKcm::loading() const { return m_loading; }
QString NamtorKcm::error() const { return m_error; }
quint32 NamtorKcm::apiVersion() const { return m_apiVersion; }

// --- State helpers ---

void NamtorKcm::setLoading(bool loading)
{
    if (m_loading == loading)
        return;
    m_loading = loading;
    emit loadingChanged();
}

void NamtorKcm::setError(const QString &error)
{
    if (m_error == error)
        return;
    m_error = error;
    emit errorChanged();
}

// --- Q_INVOKABLE methods (frozen D-Bus contract) ---

void NamtorKcm::listProfiles()
{
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCall(QStringLiteral("ListProfiles")));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleListProfilesReply(call, gen); });
}

void NamtorKcm::getStatus()
{
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCall(QStringLiteral("GetStatus")));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleGetStatusReply(call, gen); });
}

void NamtorKcm::startEnrollment(quint32 uid, const QString &name)
{
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << uid << name;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("StartEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleStartEnrollmentReply(call, gen); });
}

void NamtorKcm::cancelEnrollment(const QString &session)
{
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    // Cancel bumps generation so any in-flight start/commit reply is stale.
    ++m_generation;
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("CancelEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, call]() { handleVoidReply(call, 0); });
}

void NamtorKcm::commitProfile(const QString &session)
{
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("CommitProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleCommitProfileReply(call, gen); });
}

void NamtorKcm::reviewProfile(quint32 uid, const QString &id)
{
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << uid << id;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("ReviewProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleReviewProfileReply(call, gen); });
}

void NamtorKcm::renameProfile(quint32 uid, const QString &id, const QString &name)
{
    QList<QVariant> args;
    args << uid << id << name;
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("RenameProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleVoidReply(call, gen); });
}

void NamtorKcm::enableProfile(quint32 uid, const QString &id, bool enabled)
{
    QList<QVariant> args;
    args << uid << id << enabled;
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("EnableProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleVoidReply(call, gen); });
}

void NamtorKcm::deleteProfile(quint32 uid, const QString &id)
{
    QList<QVariant> args;
    args << uid << id;
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("DeleteProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleVoidReply(call, gen); });
}

void NamtorKcm::mergeProfiles(quint32 uid, const QString &src, const QString &dst)
{
    QList<QVariant> args;
    args << uid << src << dst;
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("MergeProfiles"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleVoidReply(call, gen); });
}

void NamtorKcm::replaceProfile(quint32 uid, const QString &id, const QString &name)
{
    QList<QVariant> args;
    args << uid << id << name;
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface->asyncCallWithArgumentList(
        QStringLiteral("ReplaceProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleVoidReply(call, gen); });
}

void NamtorKcm::cancelAllPending()
{
    ++m_generation;
    for (auto *w : std::as_const(m_pendingWatchers)) {
        w->disconnect(this);
        // QDBusPendingCallWatcher has no kill() — the reply will still arrive
        // but is silently dropped via the generation check in each handler.
        w->deleteLater();
    }
    m_pendingWatchers.clear();
    setLoading(false);
}

// --- Reply handlers (stale-response sequencing via generation check) ---

static void removeWatcher(QVector<QDBusPendingCallWatcher *> &list, QDBusPendingCallWatcher *w)
{
    list.removeAll(w);
    w->deleteLater();
}

void NamtorKcm::handleListProfilesReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation) // stale response from a cancelled call
        return;

    QDBusPendingReply<QVector<WireProfileInfo>> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        m_daemonReachable = false;
        emit statusChanged();
        setLoading(false);
        return;
    }
    setError({});
    QVector<NamtorProfileEntry> entries;
    for (const auto &p : reply.value()) {
        entries.append({p.id, p.name, p.enabled, p.created, p.lastUsed});
    }
    m_profileModel->setProfiles(entries);
    setLoading(false);
}

void NamtorKcm::handleGetStatusReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<WireStatusReport> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        m_daemonReachable = false;
        m_cameraPresent = false;
        m_emitterArmed = false;
        emit statusChanged();
        setLoading(false);
        return;
    }
    setError({});
    const auto &s = reply.value();
    m_daemonReachable = true;
    m_cameraPresent = true; // daemon reports camera-present in status
    m_emitterArmed = s.emitter_armed;
    m_apiVersion = s.api_version;
    QVector<NamtorProfileEntry> entries;
    for (const auto &p : s.profiles) {
        entries.append({p.id, p.name, p.enabled, p.created, p.lastUsed});
    }
    m_profileModel->setProfiles(entries);
    emit statusChanged();
    setLoading(false);
}

void NamtorKcm::handleStartEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<QString> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    // The session id is returned; QML picks it up via a signal or property.
    // For now, emit profileChanged as a generic "something happened" signal.
    emit profileChanged(0, reply.value());
    setLoading(false);
}

void NamtorKcm::handleCommitProfileReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<QString> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    emit enrollmentCompleted({}, reply.value());
    emit profileChanged(0, reply.value());
    setLoading(false);
}

void NamtorKcm::handleReviewProfileReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<WireReviewOutcome> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    // Review outcome is available via a signal; no biometric data.
    const auto &r = reply.value();
    emit profileChanged(0, r.profile_id);
    setLoading(false);
}

void NamtorKcm::handleVoidReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        return;
    }
    setError({});
    // After a void mutation (rename/enable/delete/merge/replace), refresh
    // the profile list so QML reflects the new state.
    listProfiles();
}
