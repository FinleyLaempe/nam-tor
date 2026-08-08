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
// D-Bus signal forwarding: the four frozen Manager signals are connected in
// the constructor via QDBusConnection::connect(). EnrollmentProgress updates
// the cached progress properties (framesCaptured, qualityLabel,
// coveragePercent) and re-emits as a Qt signal. ProfileChanged triggers an
// automatic listProfiles() refresh so external mutations are reflected.
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
#include <QDBusConnection>

#include <unistd.h>

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
    , m_currentUid(static_cast<quint32>(getuid()))
    , m_enrollmentActive(false)
    , m_framesCaptured(0)
    , m_coveragePercent(0)
    , m_reviewNeedsReenroll(false)
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

    connectDBusSignals();
}

NamtorKcm::~NamtorKcm()
{
    cancelAllPending();
}

// --- D-Bus signal forwarding ---

void NamtorKcm::connectDBusSignals()
{
    auto bus = QDBusConnection::sessionBus();
    const QString service = QString::fromUtf8(namtor_kde_manager_bus_name());
    const QString path = QString::fromUtf8(namtor_kde_manager_object_path());
    const QString iface = QStringLiteral("org.namtor.Manager");

    // Connect to each frozen Manager signal. Slots take QDBusMessage so the
    // wire structs (WireEnrollmentProgress, WireStatusReport) don't need to
    // be visible in the header — we parse the message arguments here.
    bus.connect(service, path, iface, QStringLiteral("EnrollmentProgress"),
        this, SLOT(onEnrollmentProgressDBus(QDBusMessage)));
    bus.connect(service, path, iface, QStringLiteral("EnrollmentCompleted"),
        this, SLOT(onEnrollmentCompletedDBus(QDBusMessage)));
    bus.connect(service, path, iface, QStringLiteral("ProfileChanged"),
        this, SLOT(onProfileChangedDBus(QDBusMessage)));
    bus.connect(service, path, iface, QStringLiteral("StatusChanged"),
        this, SLOT(onStatusChangedDBus(QDBusMessage)));
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

// --- Enrollment state properties ---

quint32 NamtorKcm::currentUid() const { return m_currentUid; }

QString NamtorKcm::currentSession() const { return m_currentSession; }

bool NamtorKcm::enrollmentActive() const { return m_enrollmentActive; }

quint32 NamtorKcm::framesCaptured() const { return m_framesCaptured; }

QString NamtorKcm::qualityLabel() const { return m_qualityLabel; }

quint32 NamtorKcm::coveragePercent() const { return m_coveragePercent; }

// --- Review state properties ---

QString NamtorKcm::reviewProfileId() const { return m_reviewProfileId; }

bool NamtorKcm::reviewNeedsReenroll() const { return m_reviewNeedsReenroll; }

QString NamtorKcm::reviewQuality() const { return m_reviewQuality; }

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

void NamtorKcm::setCurrentSession(const QString &session)
{
    if (m_currentSession == session)
        return;
    m_currentSession = session;
    emit currentSessionChanged();
}

void NamtorKcm::setEnrollmentActive(bool active)
{
    if (m_enrollmentActive == active)
        return;
    m_enrollmentActive = active;
    emit enrollmentActiveChanged();
}

// --- Validation helpers ---

bool NamtorKcm::isNameValid(const QString &name) const
{
    const auto trimmed = name.trimmed();
    if (trimmed.isEmpty())
        return false;
    if (trimmed.length() > 64)
        return false;
    return true;
}

bool NamtorKcm::isNameDuplicate(const QString &name, const QString &excludeId) const
{
    const auto trimmed = name.trimmed();
    const auto &profiles = m_profileModel->profiles();
    for (const auto &p : profiles) {
        if (p.id == excludeId)
            continue;
        if (p.name.compare(trimmed, Qt::CaseInsensitive) == 0)
            return true;
    }
    return false;
}

QString NamtorKcm::profileNameById(const QString &id) const
{
    const auto &profiles = m_profileModel->profiles();
    for (const auto &p : profiles) {
        if (p.id == id)
            return p.name;
    }
    return {};
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
    setEnrollmentActive(false);
    setCurrentSession({});
    m_framesCaptured = 0;
    m_coveragePercent = 0;
    m_qualityLabel.clear();
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
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
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
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
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
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
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
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
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
    if (!m_abiOk || !m_iface || !m_iface->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
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
        w->deleteLater();
    }
    m_pendingWatchers.clear();
    setLoading(false);
}

// --- D-Bus signal handlers ---

void NamtorKcm::onEnrollmentProgressDBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.size() < 2)
        return;
    const QString session = args.at(0).toString();
    const auto arg = args.at(1).value<QDBusArgument>();
    WireEnrollmentProgress p;
    arg >> p;

    m_framesCaptured = p.frames_captured;
    m_qualityLabel = p.quality_label;
    m_coveragePercent = p.coverage_percent;

    if (!m_currentSession.isEmpty() && session == m_currentSession)
        setEnrollmentActive(true);

    emit enrollmentProgress(session, p.frames_captured, p.quality_label,
                            p.coverage_percent);
}

void NamtorKcm::onEnrollmentCompletedDBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.size() < 2)
        return;
    const QString session = args.at(0).toString();
    const QString profile = args.at(1).toString();

    if (!m_currentSession.isEmpty() && session == m_currentSession) {
        setEnrollmentActive(false);
        setCurrentSession({});
    }
    emit enrollmentCompleted(session, profile);
}

void NamtorKcm::onProfileChangedDBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.size() < 2)
        return;
    const quint32 uid = args.at(0).toUInt();
    const QString id = args.at(1).toString();

    emit profileChanged(uid, id);
    listProfiles();
}

void NamtorKcm::onStatusChangedDBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.isEmpty())
        return;
    const auto arg = args.at(0).value<QDBusArgument>();
    WireStatusReport s;
    arg >> s;

    m_daemonReachable = true;
    m_cameraPresent = true;
    m_emitterArmed = s.emitter_armed;
    m_apiVersion = s.api_version;

    QVector<NamtorProfileEntry> entries;
    for (const auto &p : s.profiles) {
        entries.append({p.id, p.name, p.enabled, p.created, p.lastUsed});
    }
    m_profileModel->setProfiles(entries);

    emit statusChanged();
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
    if (gen != m_generation)
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
    m_cameraPresent = true;
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
        setEnrollmentActive(false);
        setLoading(false);
        return;
    }
    setError({});
    const QString session = reply.value();
    setCurrentSession(session);
    setEnrollmentActive(true);
    emit enrollmentStarted(session);
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
    const QString profile = reply.value();
    setEnrollmentActive(false);
    setCurrentSession({});
    emit enrollmentCompleted({}, profile);
    emit profileChanged(0, profile);
    // Refresh the list so the new profile appears.
    listProfiles();
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
    const auto &r = reply.value();
    m_reviewProfileId = r.profile_id;
    m_reviewNeedsReenroll = r.needs_reenroll;
    m_reviewQuality = r.quality;
    emit reviewReady(r.profile_id, r.needs_reenroll, r.quality);
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
        setLoading(false);
        return;
    }
    setError({});
    // After a void mutation (rename/enable/delete/merge/replace), refresh
    // the profile list so QML reflects the new state.
    listProfiles();
}
