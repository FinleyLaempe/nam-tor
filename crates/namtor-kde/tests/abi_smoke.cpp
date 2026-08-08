// SPDX-License-Identifier: GPL-3.0-only
//
// abi_smoke.cpp — C-side counterpart to the Rust test
// `abi_version_and_strings_match_bridge_constants`. Compiled and run via
// CTest (see CMakeLists.txt `add_test(namtor-abi-smoke ...)`); links against
// the Rust cdylib (`libnamtor_kde.so`) and asserts:
//   1. `namtor_kde_abi_version()` returns `NAMTOR_KDE_ABI_VERSION` (catches a
//      cdylib/header mismatch at install time).
//   2. The two D-Bus address strings are non-null, non-empty, and equal the
//      canonical `org.namtor.Manager` / `/org/namtor/Manager` values (catches a
//      silent rename on the Rust side that the header did not pick up).
//
// No biometric data is read. Exit 0 on success, non-zero on any mismatch.

#include "namtor_kde.h"

#include <cstdio>
#include <cstring>
#include <cstdlib>

static int check(bool cond, const char *msg)
{
    if (!cond) {
        std::fprintf(stderr, "ABI SMOKE FAIL: %s\n", msg);
        return 1;
    }
    return 0;
}

int main()
{
    int rc = 0;

    rc |= check(namtor_kde_abi_version() == NAMTOR_KDE_ABI_VERSION,
                "abi version mismatch between header and cdylib");

    const char *bus = namtor_kde_manager_bus_name();
    rc |= check(bus != nullptr, "manager bus name is null");
    rc |= check(std::strcmp(bus, "org.namtor.Manager") == 0,
                "manager bus name is not org.namtor.Manager");

    const char *path = namtor_kde_manager_object_path();
    rc |= check(path != nullptr, "manager object path is null");
    rc |= check(std::strcmp(path, "/org/namtor/Manager") == 0,
                "manager object path is not /org/namtor/Manager");

    if (rc == 0) {
        std::printf("ABI SMOKE OK: version=%u bus=%s path=%s\n",
                    namtor_kde_abi_version(), bus, path);
    }
    return rc;
}
