/* Windows.h case-correction forwarder for mingw-w64.
 *
 * Upstream WDSP does `#include <Windows.h>` (capital W). mingw-w64
 * ships the header as lowercase `windows.h`, so on case-sensitive
 * filesystems (Linux cross-compile) the lookup fails outright. On
 * NTFS (case-insensitive) we still want a deterministic path —
 * `<Windows.h>` always resolves here, and we forward to mingw's
 * real header.
 *
 * `#include <windows.h>` cannot be used to forward: on NTFS it would
 * match this very file again (case-insensitive) and the include guard
 * would no-op it, so mingw's header would never be pulled in.
 * `#include_next` (GCC/Clang) resumes the search *after* the directory
 * that supplied the current file, skipping this shim and reaching the
 * w32api copy.
 */
#ifndef WDSP_SHIM_WIN_WINDOWS_H
#define WDSP_SHIM_WIN_WINDOWS_H
#include_next <windows.h>
#endif
