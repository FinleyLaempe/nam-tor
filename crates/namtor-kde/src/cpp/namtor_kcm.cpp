// SPDX-License-Identifier: GPL-3.0-only
//
// namtor_kcm.cpp — async D-Bus bridge implementation for the Nam-tor KCM.
//
// All D-Bus calls go through QDBusInterface::asyncCall() + QDBusPendingCallWatcher
// on the SYSTEM bus (QDBusConnection::systemBus). The daemon is a system
// service; the KCM never used the session bus.
//
// The watcher's finished() signal fires on the GUI thread when the reply
// arrives (or times out) — the GUI never blocks. Each call carries a
// generation counter; if the user cancels or restarts, the generation is
// bumped and late replies from the old generation are silently dropped
// (stale-response sequencing).
//
// D-Bus signal forwarding: the four frozen v1 Manager signals plus the two
// v2 Manager2 signals are connected in the constructor via
// QDBusConnection::systemBus().connect(). EnrollmentProgress updates the
// cached progress properties (framesCaptured, qualityLabel, coveragePercent)
// and re-emits as a Qt signal. ProfileChanged triggers an automatic
// listProfiles() refresh so external mutations are reflected. The v2
// signals update the PAM services list and protection status respectively.
//
// No biometric data is handled here. The QObject carries only profile
// metadata, coarse quality buckets, and aggregated status — all from the
// frozen Task 8 (v1) and Task 1 (v2) contracts.

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

// --- v2 D-Bus wire structs (frozen Task 1 contract: org.namtor.Manager2) ---

struct WirePamServiceInfo {
    QString id;
    QString display_name;
    bool enabled;
    QString compatibility;
    QString description;
};
Q_DECLARE_METATYPE(WirePamServiceInfo)

struct WireProtectionStatus {
    QString sealing;
    bool has_tpm;
    QString warning;
};
Q_DECLARE_METATYPE(WireProtectionStatus)

// --- v3 D-Bus wire structs (frozen enrollment-redesign contract: org.namtor.Manager3) ---

struct WireEnrollmentProgressV3 {
    QString session;
    QString phase;
    quint32 quality_percent;
    quint32 diversity_percent;
    quint32 frames_seen;
    quint32 frames_accepted;
    quint32 frames_rejected;
    quint8 quality_bucket;
    QString guidance;
};
Q_DECLARE_METATYPE(WireEnrollmentProgressV3)

struct WireEnrollmentReviewV3 {
    QString session;
    bool ready_to_save;
    quint32 quality_percent;
    quint32 diversity_percent;
    quint32 frames_accepted;
    QString guidance;
};
Q_DECLARE_METATYPE(WireEnrollmentReviewV3)

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

QDBusArgument &operator<<(QDBusArgument &arg, const WirePamServiceInfo &p)
{
    arg.beginStructure();
    arg << p.id << p.display_name << p.enabled << p.compatibility << p.description;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WirePamServiceInfo &p)
{
    arg.beginStructure();
    arg >> p.id >> p.display_name >> p.enabled >> p.compatibility >> p.description;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const WireProtectionStatus &s)
{
    arg.beginStructure();
    arg << s.sealing << s.has_tpm << s.warning;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireProtectionStatus &s)
{
    arg.beginStructure();
    arg >> s.sealing >> s.has_tpm >> s.warning;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const WireEnrollmentProgressV3 &p)
{
    arg.beginStructure();
    arg << p.session << p.phase << p.quality_percent << p.diversity_percent
        << p.frames_seen << p.frames_accepted << p.frames_rejected
        << p.quality_bucket << p.guidance;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireEnrollmentProgressV3 &p)
{
    arg.beginStructure();
    arg >> p.session >> p.phase >> p.quality_percent >> p.diversity_percent
        >> p.frames_seen >> p.frames_accepted >> p.frames_rejected
        >> p.quality_bucket >> p.guidance;
    arg.endStructure();
    return arg;
}

QDBusArgument &operator<<(QDBusArgument &arg, const WireEnrollmentReviewV3 &r)
{
    arg.beginStructure();
    arg << r.session << r.ready_to_save << r.quality_percent
        << r.diversity_percent << r.frames_accepted << r.guidance;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, WireEnrollmentReviewV3 &r)
{
    arg.beginStructure();
    arg >> r.session >> r.ready_to_save >> r.quality_percent
        >> r.diversity_percent >> r.frames_accepted >> r.guidance;
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
    qRegisterMetaType<WirePamServiceInfo>("WirePamServiceInfo");
    qDBusRegisterMetaType<WirePamServiceInfo>();
    qRegisterMetaType<QVector<WirePamServiceInfo>>("QVector<WirePamServiceInfo>");
    qDBusRegisterMetaType<QVector<WirePamServiceInfo>>();
    qRegisterMetaType<WireProtectionStatus>("WireProtectionStatus");
    qDBusRegisterMetaType<WireProtectionStatus>();
    qRegisterMetaType<WireEnrollmentProgressV3>("WireEnrollmentProgressV3");
    qDBusRegisterMetaType<WireEnrollmentProgressV3>();
    qRegisterMetaType<WireEnrollmentReviewV3>("WireEnrollmentReviewV3");
    qDBusRegisterMetaType<WireEnrollmentReviewV3>();
    s_typesRegistered = true;
}

NamtorKcm::NamtorKcm(QObject *parent)
    : QObject(parent)
    , m_abiOk(false)
    , m_profileModel(new NamtorProfileModel(this))
    , m_iface(nullptr)
    , m_iface2(nullptr)
    , m_iface3(nullptr)
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
    , m_protectionHasTpm(false)
    , m_qualityPercent(0)
    , m_diversityPercent(0)
    , m_framesSeen(0)
    , m_framesAccepted(0)
    , m_framesRejected(0)
    , m_qualityBucket(0)
    , m_reviewReadyToSave(false)
    , m_reviewQualityPercent(0)
    , m_reviewDiversityPercent(0)
    , m_reviewFramesAccepted(0)
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
        QDBusConnection::systemBus(),
        this);

    m_iface2 = new QDBusInterface(
        QString::fromUtf8(namtor_kde_manager_bus_name()),
        QString::fromUtf8(namtor_kde_manager_object_path()),
        QStringLiteral("org.namtor.Manager2"),
        QDBusConnection::systemBus(),
        this);

    m_iface3 = new QDBusInterface(
        QString::fromUtf8(namtor_kde_manager_bus_name()),
        QString::fromUtf8(namtor_kde_manager_object_path()),
        QStringLiteral("org.namtor.Manager3"),
        QDBusConnection::systemBus(),
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
    auto bus = QDBusConnection::systemBus();
    const QString service = QString::fromUtf8(namtor_kde_manager_bus_name());
    const QString path = QString::fromUtf8(namtor_kde_manager_object_path());
    const QString iface = QStringLiteral("org.namtor.Manager");
    const QString iface2 = QStringLiteral("org.namtor.Manager2");
    const QString iface3 = QStringLiteral("org.namtor.Manager3");

    bus.connect(service, path, iface, QStringLiteral("EnrollmentProgress"),
        this, SLOT(onEnrollmentProgressDBus(QDBusMessage)));
    bus.connect(service, path, iface, QStringLiteral("EnrollmentCompleted"),
        this, SLOT(onEnrollmentCompletedDBus(QDBusMessage)));
    bus.connect(service, path, iface, QStringLiteral("ProfileChanged"),
        this, SLOT(onProfileChangedDBus(QDBusMessage)));
    bus.connect(service, path, iface, QStringLiteral("StatusChanged"),
        this, SLOT(onStatusChangedDBus(QDBusMessage)));

    bus.connect(service, path, iface2, QStringLiteral("PamServicesChanged"),
        this, SLOT(onPamServicesChangedDBus(QDBusMessage)));
    bus.connect(service, path, iface2, QStringLiteral("ProtectionStatusChanged"),
        this, SLOT(onProtectionStatusChangedDBus(QDBusMessage)));

    bus.connect(service, path, iface3, QStringLiteral("EnrollmentProgress"),
        this, SLOT(onEnrollmentProgressV3DBus(QDBusMessage)));
    bus.connect(service, path, iface3, QStringLiteral("EnrollmentCompleted"),
        this, SLOT(onEnrollmentCompletedDBus(QDBusMessage)));
    bus.connect(service, path, iface3, QStringLiteral("ProfileChanged"),
        this, SLOT(onProfileChangedDBus(QDBusMessage)));
    bus.connect(service, path, iface3, QStringLiteral("StatusChanged"),
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

// --- v2 ProtectionStatus / PAM services properties ---

QString NamtorKcm::protectionSealing() const { return m_protectionSealing; }

bool NamtorKcm::protectionHasTpm() const { return m_protectionHasTpm; }

QString NamtorKcm::protectionWarning() const { return m_protectionWarning; }

QVariantList NamtorKcm::pamServices() const { return m_pamServices; }

// --- v3 enrollment state properties ---

QString NamtorKcm::enrollmentPhase() const { return m_enrollmentPhase; }
quint32 NamtorKcm::qualityPercent() const { return m_qualityPercent; }
quint32 NamtorKcm::diversityPercent() const { return m_diversityPercent; }
quint32 NamtorKcm::framesSeen() const { return m_framesSeen; }
quint32 NamtorKcm::framesAccepted() const { return m_framesAccepted; }
quint32 NamtorKcm::framesRejected() const { return m_framesRejected; }
quint8 NamtorKcm::qualityBucket() const { return m_qualityBucket; }
QString NamtorKcm::guidance() const { return m_guidance; }

// --- v3 review state properties ---

bool NamtorKcm::reviewReadyToSave() const { return m_reviewReadyToSave; }
quint32 NamtorKcm::reviewQualityPercent() const { return m_reviewQualityPercent; }
quint32 NamtorKcm::reviewDiversityPercent() const { return m_reviewDiversityPercent; }
quint32 NamtorKcm::reviewFramesAccepted() const { return m_reviewFramesAccepted; }
QString NamtorKcm::reviewGuidance() const { return m_reviewGuidance; }

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

// --- v2 PAM high-impact helpers ---

QString NamtorKcm::highImpactWarning() const
{
    return QStringLiteral(
        "FAR/FRR and real-attack PAD gates remain unmeasured. "
        "Enabling face authentication is experimental and "
        "not recommended for security-sensitive use.");
}

bool NamtorKcm::isHighImpact(const QString &compatibility) const
{
    return compatibility.compare(QStringLiteral("risky"), Qt::CaseInsensitive) == 0;
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

// --- v2 Q_INVOKABLE methods (org.namtor.Manager2) ---

void NamtorKcm::listPamServices()
{
    if (!m_abiOk || !m_iface2 || !m_iface2->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface2->asyncCall(QStringLiteral("ListPamServices")));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleListPamServicesReply(call, gen); });
}

void NamtorKcm::setPamServiceEnabled(const QString &serviceId, bool enabled)
{
    if (!m_abiOk || !m_iface2 || !m_iface2->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    QList<QVariant> args;
    args << serviceId << enabled;
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface2->asyncCallWithArgumentList(
        QStringLiteral("SetPamServiceEnabled"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleSetPamServiceEnabledReply(call, gen); });
}

void NamtorKcm::getProtectionStatus()
{
    if (!m_abiOk || !m_iface2 || !m_iface2->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    auto *call = new QDBusPendingCallWatcher(m_iface2->asyncCall(QStringLiteral("GetProtectionStatus")));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleGetProtectionStatusReply(call, gen); });
}

// --- v3 Q_INVOKABLE methods (org.namtor.Manager3 continuous enrollment) ---

void NamtorKcm::startEnrollmentV3(quint32 uid, const QString &name)
{
    if (!m_abiOk || !m_iface3 || !m_iface3->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << uid << name;
    auto *call = new QDBusPendingCallWatcher(m_iface3->asyncCallWithArgumentList(
        QStringLiteral("StartEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleStartEnrollmentV3Reply(call, gen); });
}

void NamtorKcm::stopEnrollment(const QString &session)
{
    if (!m_abiOk || !m_iface3 || !m_iface3->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface3->asyncCallWithArgumentList(
        QStringLiteral("StopEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleStopEnrollmentReply(call, gen); });
}

void NamtorKcm::resumeEnrollment(const QString &session)
{
    if (!m_abiOk || !m_iface3 || !m_iface3->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface3->asyncCallWithArgumentList(
        QStringLiteral("ResumeEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleResumeEnrollmentReply(call, gen); });
}

void NamtorKcm::reviewEnrollment(const QString &session)
{
    if (!m_abiOk || !m_iface3 || !m_iface3->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface3->asyncCallWithArgumentList(
        QStringLiteral("ReviewEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleReviewEnrollmentReply(call, gen); });
}

void NamtorKcm::commitProfileV3(const QString &session)
{
    if (!m_abiOk || !m_iface3 || !m_iface3->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    setLoading(true);
    const quint64 gen = ++m_generation;
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface3->asyncCallWithArgumentList(
        QStringLiteral("CommitProfile"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, gen, call]() { handleCommitProfileV3Reply(call, gen); });
}

void NamtorKcm::cancelEnrollmentV3(const QString &session)
{
    if (!m_abiOk || !m_iface3 || !m_iface3->isValid()) {
        setError(QStringLiteral("daemon unavailable"));
        return;
    }
    ++m_generation;
    setEnrollmentActive(false);
    setCurrentSession({});
    m_enrollmentPhase.clear();
    m_qualityPercent = 0;
    m_diversityPercent = 0;
    m_framesSeen = 0;
    m_framesAccepted = 0;
    m_framesRejected = 0;
    m_qualityBucket = 0;
    m_guidance.clear();
    QList<QVariant> args;
    args << session;
    auto *call = new QDBusPendingCallWatcher(m_iface3->asyncCallWithArgumentList(
        QStringLiteral("CancelEnrollment"), args));
    m_pendingWatchers.append(call);
    connect(call, &QDBusPendingCallWatcher::finished, this,
            [this, call]() { handleCancelEnrollmentV3Reply(call, 0); });
}

void NamtorKcm::refresh()
{
    listProfiles();
    getStatus();
    listPamServices();
    getProtectionStatus();
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

void NamtorKcm::onPamServicesChangedDBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.isEmpty())
        return;
    const auto arg = args.at(0).value<QDBusArgument>();
    QVector<WirePamServiceInfo> services;
    arg >> services;

    QVariantList out;
    for (const auto &s : services) {
        QVariantMap m;
        m.insert(QStringLiteral("id"), s.id);
        m.insert(QStringLiteral("displayName"), s.display_name);
        m.insert(QStringLiteral("enabled"), s.enabled);
        m.insert(QStringLiteral("compatibility"), s.compatibility);
        m.insert(QStringLiteral("description"), s.description);
        out.append(m);
    }
    m_pamServices = out;
    emit pamServicesChanged();
}

void NamtorKcm::onProtectionStatusChangedDBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.isEmpty())
        return;
    const auto arg = args.at(0).value<QDBusArgument>();
    WireProtectionStatus s;
    arg >> s;

    m_protectionSealing = s.sealing;
    m_protectionHasTpm = s.has_tpm;
    m_protectionWarning = s.warning;
    emit protectionStatusChanged();
}

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

void NamtorKcm::handleListPamServicesReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<QVector<WirePamServiceInfo>> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    QVariantList out;
    for (const auto &s : reply.value()) {
        QVariantMap m;
        m.insert(QStringLiteral("id"), s.id);
        m.insert(QStringLiteral("displayName"), s.display_name);
        m.insert(QStringLiteral("enabled"), s.enabled);
        m.insert(QStringLiteral("compatibility"), s.compatibility);
        m.insert(QStringLiteral("description"), s.description);
        out.append(m);
    }
    m_pamServices = out;
    emit pamServicesChanged();
    setLoading(false);
}

void NamtorKcm::handleSetPamServiceEnabledReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<bool> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    // The daemon emits PamServicesChanged on success; refresh now so the UI
    // reflects the toggle even if the signal arrives slightly later.
    listPamServices();
    setLoading(false);
}

void NamtorKcm::handleGetProtectionStatusReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<WireProtectionStatus> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    const auto &s = reply.value();
    m_protectionSealing = s.sealing;
    m_protectionHasTpm = s.has_tpm;
    m_protectionWarning = s.warning;
    emit protectionStatusChanged();
    setLoading(false);
}

// --- v3 reply handlers (org.namtor.Manager3) ---

void NamtorKcm::handleStartEnrollmentV3Reply(QDBusPendingCallWatcher *watcher, quint64 gen)
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
    m_enrollmentPhase = QStringLiteral("capturing");
    emit enrollmentStarted(session);
    emit enrollmentProgressV3(session, m_enrollmentPhase, 0, 0, 0, 0, 0, 0, QString());
    setLoading(false);
}

void NamtorKcm::handleStopEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen)
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
    m_enrollmentPhase = QStringLiteral("stopped");
    emit enrollmentStopped(m_currentSession);
    setLoading(false);
}

void NamtorKcm::handleResumeEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen)
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
    m_enrollmentPhase = QStringLiteral("capturing");
    emit enrollmentResumed(m_currentSession);
    setLoading(false);
}

void NamtorKcm::handleReviewEnrollmentReply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<WireEnrollmentReviewV3> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
        setLoading(false);
        return;
    }
    setError({});
    const auto &r = reply.value();
    m_reviewReadyToSave = r.ready_to_save;
    m_reviewQualityPercent = r.quality_percent;
    m_reviewDiversityPercent = r.diversity_percent;
    m_reviewFramesAccepted = r.frames_accepted;
    m_reviewGuidance = r.guidance;
    emit reviewReadyV3(r.session, r.ready_to_save, r.quality_percent,
                       r.diversity_percent, r.frames_accepted, r.guidance);
    setLoading(false);
}

void NamtorKcm::handleCommitProfileV3Reply(QDBusPendingCallWatcher *watcher, quint64 gen)
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
    m_enrollmentPhase.clear();
    emit enrollmentCompleted({}, profile);
    emit profileChanged(0, profile);
    listProfiles();
    setLoading(false);
}

void NamtorKcm::handleCancelEnrollmentV3Reply(QDBusPendingCallWatcher *watcher, quint64 gen)
{
    removeWatcher(m_pendingWatchers, watcher);
    if (gen != m_generation)
        return;

    QDBusPendingReply<> reply = *watcher;
    if (reply.isError()) {
        setError(reply.error().message());
    }
    setError({});
}

// --- v3 D-Bus signal handler ---

void NamtorKcm::onEnrollmentProgressV3DBus(const QDBusMessage &msg)
{
    const auto args = msg.arguments();
    if (args.isEmpty())
        return;
    const auto arg = args.at(0).value<QDBusArgument>();
    WireEnrollmentProgressV3 p;
    arg >> p;

    if (!m_currentSession.isEmpty() && p.session != m_currentSession)
        return;

    m_enrollmentPhase = p.phase;
    m_qualityPercent = p.quality_percent;
    m_diversityPercent = p.diversity_percent;
    m_framesSeen = p.frames_seen;
    m_framesAccepted = p.frames_accepted;
    m_framesRejected = p.frames_rejected;
    m_qualityBucket = p.quality_bucket;
    m_guidance = p.guidance;

    if (p.phase == QStringLiteral("capturing") && !m_currentSession.isEmpty())
        setEnrollmentActive(true);

    emit enrollmentProgressV3(p.session, p.phase, p.quality_percent,
                              p.diversity_percent, p.frames_seen,
                              p.frames_accepted, p.frames_rejected,
                              p.quality_bucket, p.guidance);
}
