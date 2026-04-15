/* Preprocessor stub force-included only on mingw targets.
 * mingw's <string.h> doesn't declare stpcpy (GNU/POSIX extension),
 * but ft8_lib's message.c calls it unconditionally. Declaring it
 * here lets the call sites compile without an implicit-int
 * conversion error; the implementation lives in shim.c. */
#ifndef ARION_FT8_MINGW_COMPAT_H
#define ARION_FT8_MINGW_COMPAT_H
#include <stddef.h>
char *stpcpy(char *dst, const char *src);
#endif
