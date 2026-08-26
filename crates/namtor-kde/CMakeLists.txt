# SPDX-License-Identifier: GPL-3.0-only
#
# CMakeLists.txt — reproducible Plasma 6 / KF6 build for the Nam-tor KCM.
#
# Produces:
#   1. `org.namtor` QML module — the C++ shim (`NamtorKcm` singleton) plus
#      `main.qml`, built via `qt_add_qml_module`. System Settings / kcmshell6
#      load `main.qml` which imports `org.namtor 1.0` (spec §9.3).
#   2. `metadata.json` — KPlugin JSON (Plasma 6 format) installed alongside
#      the module so System Settings discovers the KCM.
#   3. `namtor-abi-smoke` — a CTest binary that links the Rust cdylib and
#      asserts the C ABI version/strings match the header.
#
# The Rust cdylib (`libnamtor_kde.so`) is built by `cargo build -p namtor-kde`
# and located via `NAMTOR_KDE_RUST_SO` (set by the packaging layer or
# auto-discovered from the workspace `target/` dir).
#
# Pin policy: every `find_package` pins a minimum version to keep the build
# reproducible across distro snapshots. Qt 6.6 is the KF6 baseline; KF6
# frameworks pin to 6.0 (earliest KF6 release).

cmake_minimum_required(VERSION 3.28)

project(namtor-kde
    VERSION 0.1.0
    DESCRIPTION "Nam-tor face authentication KCM (Plasma 6)"
    LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
# AUTOMOC is required by qt_add_qml_module for QML_ELEMENT metatype generation.
set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTORCC ON)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE Release CACHE STRING "Build type" FORCE)
endif()

# --- Qt6 (pinned to KF6 baseline) --------------------------------------------

find_package(Qt6 6.6 REQUIRED COMPONENTS Core Gui Qml DBus)

# --- KF6 frameworks (individual configs; KF6 ships no meta-package) ----------

# Kirigami: QML controls used by main.qml (runtime QML import; the build dep
# is for the kirigami QML import path so qt_add_qml_module can find it).
find_package(KF6Kirigami 6.0)
# CoreAddons: KPlugin metadata + (optionally) kcoreaddons_add_plugin.
find_package(KF6CoreAddons 6.0)
# I18n: i18n() in QML (runtime QML import).
find_package(KF6I18n 6.0)

# KCMUtils + ECM are required for the full KCM plugin target
# (kcoreaddons_add_plugin + the QML KCM QML types). They are optional here so
# the abi-smoke test and the QML module still build on a host that has Qt6 +
# KF6Kirigami but not the full KDE development headers. The packaging layer
# (install-manifest) asserts their presence at package-build time.
find_package(KF6KCMUtils 6.0 QUIET)
find_package(ECM 6.0 QUIET NO_MODULE)

set(BUILD_KCM_PLUGIN OFF)
if(ECM_FOUND)
    set(CMAKE_MODULE_PATH ${CMAKE_MODULE_PATH} ${ECM_MODULE_PATH})
    include(KDEInstallDirs)
endif()
if(KF6KCMUtils_FOUND AND ECM_FOUND AND KF6CoreAddons_FOUND)
    set(BUILD_KCM_PLUGIN ON)
elseif(NOT KF6KCMUtils_FOUND OR NOT ECM_FOUND)
    message(STATUS
        "KCM plugin build disabled (KF6KCMUtils_FOUND=${KF6KCMUtils_FOUND} "
        "ECM_FOUND=${ECM_FOUND}). Only the QML module + abi-smoke test will "
        "be built. Install extra-cmake-modules + kf6-kcmutils for the full KCM.")
endif()

# --- Rust cdylib location ----------------------------------------------------

# The packaging layer passes -DNAMTOR_KDE_RUST_SO=/path/to/libnamtor_kde.so.
# Fall back to the workspace target dir so a developer build works after
# `cargo build -p namtor-kde`.
if(NOT DEFINED NAMTOR_KDE_RUST_SO)
    set(_candidate_debug "${CMAKE_SOURCE_DIR}/../../target/debug/libnamtor_kde.so")
    set(_candidate_release "${CMAKE_SOURCE_DIR}/../../target/release/libnamtor_kde.so")
    if(EXISTS "${_candidate_debug}")
        set(_default_so "${_candidate_debug}")
    elseif(EXISTS "${_candidate_release}")
        set(_default_so "${_candidate_release}")
    else()
        set(_default_so "${_candidate_debug}")
    endif()
    set(NAMTOR_KDE_RUST_SO "${_default_so}" CACHE FILEPATH
        "Path to the Rust cdylib libnamtor_kde.so (build with: cargo build -p namtor-kde)")
endif()

if(NOT EXISTS "${NAMTOR_KDE_RUST_SO}")
    message(WARNING
        "NAMTOR_KDE_RUST_SO not found at ${NAMTOR_KDE_RUST_SO}. "
        "Run `cargo build -p namtor-kde` first, or pass "
        "-DNAMTOR_KDE_RUST_SO=/path/to/libnamtor_kde.so. "
        "The abi-smoke test and KCM plugin will fail to link.")
endif()

# --- QML module (org.namtor) -------------------------------------------------

# qt_add_qml_module compiles the C++ shim (QML_ELEMENT + QML_SINGLETON) and
# the QML file into a loadable Qt6 QML module. main.qml imports
# `org.namtor 1.0` and binds `bridge` to the `NamtorKcm` singleton.
qt_add_qml_module(namtorqml
    URI org.namtor
    VERSION 1.0
    SOURCES
        src/cpp/namtor_kcm.cpp
        src/cpp/profile_model.cpp
    QML_FILES
        qml/main.qml
    NO_PLUGIN
)

target_include_directories(namtorqml PRIVATE src/cpp)

target_link_libraries(namtorqml
    PRIVATE
        Qt6::Core
        Qt6::Gui
        Qt6::Qml
        Qt6::DBus
)

if(EXISTS "${NAMTOR_KDE_RUST_SO}")
    target_link_libraries(namtorqml PRIVATE "${NAMTOR_KDE_RUST_SO}")
endif()

# --- Install rules -----------------------------------------------------------

# QML module: install to the Qt6 QML import dir so `import org.namtor 1.0`
# resolves from any Qt6 QML engine (System Settings, kcmshell6, standalone).
# KDE_INSTALL_* vars are defined by ECM's KDEInstallDirs; without ECM we
# fall back to CMAKE_INSTALL_* (GNUInstallDirs defaults).
if(ECM_FOUND)
    install(TARGETS namtorqml
        LIBRARY DESTINATION "${KDE_INSTALL_QMLDIR}/org/namtor"
        ARCHIVE DESTINATION "${KDE_INSTALL_QMLDIR}/org/namtor")
    install(FILES qml/main.qml
        DESTINATION "${KDE_INSTALL_QMLDIR}/org/namtor")
    install(FILES metadata.json
        DESTINATION "${KDE_INSTALL_METADATADIR}/kpackage/kcms/systemsettings/kcm_namtor/")
    install(FILES "${CMAKE_SOURCE_DIR}/../../packaging/applications/org.namtor.kcm.desktop"
        DESTINATION "${KDE_INSTALL_APPDIR}")
    install(FILES "${CMAKE_SOURCE_DIR}/../../packaging/metainfo/org.namtor.kcm.metainfo.xml"
        DESTINATION "${KDE_INSTALL_METAINFODIR}")
    install(FILES "${CMAKE_SOURCE_DIR}/../../packaging/icons/hicolor/scalable/apps/org.namtor.kcm.svg"
        DESTINATION "${KDE_INSTALL_ICONDIR}/hicolor/scalable/apps")
else()
    install(TARGETS namtorqml
        LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}/qt6/qml/org/namtor"
        ARCHIVE DESTINATION "${CMAKE_INSTALL_LIBDIR}/qt6/qml/org/namtor")
    install(FILES qml/main.qml
        DESTINATION "${CMAKE_INSTALL_DATADIR}/qt6/qml/org/namtor")
    install(FILES metadata.json
        DESTINATION "${CMAKE_INSTALL_DATADIR}/kpackage/kcms/systemsettings/kcm_namtor/")
    install(FILES "${CMAKE_SOURCE_DIR}/../../packaging/applications/org.namtor.kcm.desktop"
        DESTINATION "${CMAKE_INSTALL_DATADIR}/applications")
    install(FILES "${CMAKE_SOURCE_DIR}/../../packaging/metainfo/org.namtor.kcm.metainfo.xml"
        DESTINATION "${CMAKE_INSTALL_DATADIR}/metainfo")
    install(FILES "${CMAKE_SOURCE_DIR}/../../packaging/icons/hicolor/scalable/apps/org.namtor.kcm.svg"
        DESTINATION "${CMAKE_INSTALL_DATADIR}/icons/hicolor/scalable/apps")
endif()

# --- ABI smoke test (CTest) --------------------------------------------------

enable_testing()

add_executable(namtor-abi-smoke tests/abi_smoke.cpp)
target_include_directories(namtor-abi-smoke PRIVATE src/cpp)
if(EXISTS "${NAMTOR_KDE_RUST_SO}")
    target_link_libraries(namtor-abi-smoke PRIVATE "${NAMTOR_KDE_RUST_SO}")
endif()

add_test(NAME namtor-abi-smoke COMMAND namtor-abi-smoke)
