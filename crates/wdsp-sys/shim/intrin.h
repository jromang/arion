/* Intercepted <intrin.h>. WDSP only uses Interlocked* and _MM_SET_FLUSH_ZERO_MODE;
 * both are handled in wdsp_posix.h. On x86 the real xmmintrin.h is pulled in
 * transitively to get _MM_FLUSH_ZERO_ON. */
#ifndef WDSP_SHIM_INTRIN_H
#define WDSP_SHIM_INTRIN_H
#include "wdsp_posix.h"
#endif
