/* Windows.h case-correction forwarder for mingw-w64 cross-compile.
 *
 * Upstream WDSP does `#include <Windows.h>` (capital W). On real
 * Windows the NTFS-side filesystem is case-insensitive so w32api's
 * lowercase `windows.h` matches. Cross-compiling from a case-
 * sensitive Linux host breaks that assumption — the lookup fails
 * outright. This one-liner forwarder lives in `shim-win/` which the
 * build script only injects into the include path when the target
 * is Windows, so the native Linux POSIX shim in `shim/` is untouched.
 */
#ifndef WDSP_SHIM_WIN_WINDOWS_H
#define WDSP_SHIM_WIN_WINDOWS_H
#include <windows.h>
#endif
