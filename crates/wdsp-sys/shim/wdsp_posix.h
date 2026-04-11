/* wdsp_posix.h — POSIX portability shim for WDSP.
 *
 * WDSP is written against a small subset of the Win32 API (critical sections,
 * semaphores, _beginthread, a handful of Interlocked* operations, MM thread
 * priority). On non-Windows targets we intercept the upstream includes of
 * <Windows.h>, <process.h>, <intrin.h> and <avrt.h> with stub headers under
 * this crate's `shim/` directory; those stubs forward to this file.
 *
 * We intentionally keep the shim header-only for the inline-able bits and
 * push thread-spawn glue + a few non-trivial helpers into wdsp_posix.c.
 */
#ifndef WDSP_POSIX_H
#define WDSP_POSIX_H

#ifdef _WIN32
#error "wdsp_posix.h is for non-Windows targets only"
#endif

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <errno.h>
#include <pthread.h>
#include <semaphore.h>

#if defined(__x86_64__) || defined(__i386__)
#  include <xmmintrin.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* --------------------------------------------------------------------- */
/*  Basic Windows types                                                   */
/* --------------------------------------------------------------------- */

typedef int32_t  BOOL;
typedef uint32_t DWORD;
typedef int32_t  LONG;
typedef uint32_t ULONG;
typedef uint8_t  BYTE;
typedef uint16_t WORD;
typedef void*    HANDLE;
typedef void*    LPVOID;
typedef const void* LPCVOID;
typedef char     CHAR;
typedef char*    LPSTR;
typedef const char* LPCSTR;

/* Calling convention qualifiers: meaningless outside of 32-bit Windows. */
#ifndef __cdecl
#  define __cdecl
#endif
#ifndef __stdcall
#  define __stdcall
#endif
#ifndef WINAPI
#  define WINAPI
#endif
#ifndef APIENTRY
#  define APIENTRY
#endif

#ifndef TRUE
#  define TRUE  1
#endif
#ifndef FALSE
#  define FALSE 0
#endif

#ifndef INFINITE
#  define INFINITE 0xFFFFFFFFu
#endif

#ifndef WAIT_OBJECT_0
#  define WAIT_OBJECT_0 0x00000000u
#endif
#ifndef WAIT_TIMEOUT
#  define WAIT_TIMEOUT  0x00000102u
#endif
#ifndef WAIT_FAILED
#  define WAIT_FAILED   0xFFFFFFFFu
#endif

/* TEXT() is used with CreateEvent / AvSetMmThreadCharacteristics. Since those
 * become no-ops / opaque in the shim, TEXT just resolves to a plain C string. */
#ifndef TEXT
#  define TEXT(x) x
#endif

/* Marker for WDSP's exported entry points and for its occasional use of
 * `__declspec(align(N))`. WDSP spells the exported-symbol marker as:
 *     #define PORT __declspec(dllexport)
 *
 * On ELF/Mach-O we want default visibility for exports and real alignment
 * for the align form. We map via token-pasting:
 *     __declspec(dllexport)  -> __attribute__((visibility("default")))
 *     __declspec(align(16))  -> __attribute__((aligned(16)))
 *
 * If new variants creep in at upstream-update time, add them here. */
#ifndef __declspec
#  define __declspec(x) _WDSP_DECLSPEC_##x
#endif
#define _WDSP_DECLSPEC_dllexport   __attribute__((visibility("default")))
#define _WDSP_DECLSPEC_dllimport   /* nothing */
#define _WDSP_DECLSPEC_align(n)    __attribute__((aligned(n)))
#define _WDSP_DECLSPEC_noinline    __attribute__((noinline))

/* --------------------------------------------------------------------- */
/*  Critical sections -> recursive pthread_mutex_t                        */
/* --------------------------------------------------------------------- */

typedef struct {
    pthread_mutex_t mtx;
    int             initialized;
} CRITICAL_SECTION;

typedef CRITICAL_SECTION *LPCRITICAL_SECTION;

/* MSVC has `byte` as a built-in (stdint.h extension on some versions, or via
 * <rpcndr.h>). Upstream WDSP uses it exactly like `unsigned char`. */
typedef unsigned char byte;

void  _wdsp_cs_init(CRITICAL_SECTION *cs);
void  _wdsp_cs_delete(CRITICAL_SECTION *cs);

static inline void InitializeCriticalSection(CRITICAL_SECTION *cs) {
    _wdsp_cs_init(cs);
}
static inline BOOL InitializeCriticalSectionAndSpinCount(CRITICAL_SECTION *cs, DWORD spin) {
    (void)spin;
    _wdsp_cs_init(cs);
    return TRUE;
}
static inline void DeleteCriticalSection(CRITICAL_SECTION *cs) {
    _wdsp_cs_delete(cs);
}
static inline void EnterCriticalSection(CRITICAL_SECTION *cs) {
    pthread_mutex_lock(&cs->mtx);
}
static inline void LeaveCriticalSection(CRITICAL_SECTION *cs) {
    pthread_mutex_unlock(&cs->mtx);
}

/* --------------------------------------------------------------------- */
/*  Semaphores / events -> HANDLE-wrapped POSIX objects                   */
/* --------------------------------------------------------------------- */

/* HANDLE returned by CreateSemaphore/CreateEvent is an opaque owner pointer
 * managed by wdsp_posix.c. WDSP treats HANDLEs opaquely and never inspects
 * them, so we're free to pick any tagged-pointer representation we like. */

HANDLE _wdsp_sem_create(long initial_count, long max_count);
HANDLE _wdsp_event_create(BOOL manual_reset, BOOL initial_state);
DWORD  _wdsp_wait_single(HANDLE h, DWORD timeout_ms);
BOOL   _wdsp_sem_release(HANDLE h, long release_count, long *prev_count);
BOOL   _wdsp_event_set(HANDLE h);
BOOL   _wdsp_event_reset(HANDLE h);
BOOL   _wdsp_close_handle(HANDLE h);

static inline HANDLE CreateSemaphore(void *sec, long initial, long max, void *name) {
    (void)sec; (void)name;
    return _wdsp_sem_create(initial, max);
}
static inline HANDLE CreateEvent(void *sec, BOOL manual_reset, BOOL initial_state, const char *name) {
    (void)sec; (void)name;
    return _wdsp_event_create(manual_reset, initial_state);
}
static inline DWORD WaitForSingleObject(HANDLE h, DWORD ms) {
    return _wdsp_wait_single(h, ms);
}
static inline BOOL ReleaseSemaphore(HANDLE h, long release_count, long *prev_count) {
    return _wdsp_sem_release(h, release_count, prev_count);
}
static inline BOOL SetEvent(HANDLE h)   { return _wdsp_event_set(h); }
static inline BOOL ResetEvent(HANDLE h) { return _wdsp_event_reset(h); }
static inline BOOL CloseHandle(HANDLE h) { return _wdsp_close_handle(h); }

/* --------------------------------------------------------------------- */
/*  Threads                                                                */
/* --------------------------------------------------------------------- */

/* WDSP uses the MSVC _beginthread form:
 *     uintptr_t _beginthread(void (*)(void*), unsigned stack_size, void *arg);
 * Fire-and-forget; the returned handle is sometimes cast to HANDLE and passed
 * to SetThreadPriority which we also stub. */
uintptr_t _wdsp_beginthread(void (*start)(void*), unsigned stack_size, void *arg);

static inline uintptr_t _beginthread(void (*start)(void*), unsigned stack_size, void *arg) {
    return _wdsp_beginthread(start, stack_size, arg);
}

/* _endthread(): MSVC equivalent of `return from thread start routine`. On
 * POSIX that's pthread_exit; we never pass a return value because WDSP's
 * upstream _beginthread worker functions return void. */
static inline void _endthread(void) { pthread_exit(NULL); }

/* SetThreadPriority / GetCurrentThread / THREAD_PRIORITY_* — WDSP only uses
 * these to hint "make me realtime". On Linux this would require CAP_SYS_NICE
 * or pthread_setschedparam with SCHED_FIFO; we degrade to a no-op for now and
 * the scheduler decides. Promoted to phase F if real audio glitches show up. */
#define THREAD_PRIORITY_HIGHEST 2
static inline HANDLE GetCurrentThread(void) { return (HANDLE)(uintptr_t)pthread_self(); }
static inline BOOL SetThreadPriority(HANDLE thread, int prio) {
    (void)thread; (void)prio; return TRUE;
}

/* --------------------------------------------------------------------- */
/*  Aligned allocation                                                     */
/* --------------------------------------------------------------------- */

/* MSVC exposes `_aligned_malloc(size, alignment)` and `_aligned_free(ptr)`
 * from <malloc.h>. POSIX equivalents are posix_memalign + free. We wrap them
 * so every upstream call site builds unchanged. Alignment must be a power of
 * two and a multiple of sizeof(void*); WDSP always passes 16, which satisfies
 * both constraints on the targets we support. */
static inline void *_aligned_malloc(size_t size, size_t alignment) {
    if (alignment < sizeof(void *)) alignment = sizeof(void *);
    if (size == 0) size = 1;
    void *p = NULL;
    if (posix_memalign(&p, alignment, size) != 0) return NULL;
    return p;
}
static inline void _aligned_free(void *p) { free(p); }

/* --------------------------------------------------------------------- */
/*  Console + freopen_s (used by wisdom.c for MSVC-style console output) */
/* --------------------------------------------------------------------- */

/* Upstream `wisdom.c` pops up a Win32 console window, redirects stdout
 * into it with `freopen_s`, then prints FFT plan progress while WDSPwisdom
 * runs. On POSIX stdout already works and we don't want a popup, so these
 * are all no-ops. `freopen_s` is MSVC's secure-variant that writes the
 * reopened FILE* into an out parameter; we just hand back the original
 * stream unchanged. */
#include <stdio.h>
#ifndef ERRNO_T_DEFINED
#  define ERRNO_T_DEFINED
typedef int errno_t;
#endif

static inline BOOL AllocConsole(void) { return TRUE; }
static inline BOOL FreeConsole(void)  { return TRUE; }
static inline errno_t freopen_s(FILE **out_stream, const char *path,
                                const char *mode, FILE *stream) {
    (void)path; (void)mode;
    if (out_stream) *out_stream = stream;
    return 0;
}

/* `strcpy_s` / `strncat_s` / `sprintf_s` — only used in a few places in
 * upstream; they behave like their non-`_s` cousins for our purposes. */
#ifndef sprintf_s
#  define sprintf_s(buf, size, fmt, ...) snprintf((buf), (size), (fmt), ##__VA_ARGS__)
#endif

/* --------------------------------------------------------------------- */
/*  Sleep                                                                  */
/* --------------------------------------------------------------------- */

static inline void Sleep(DWORD ms) {
    struct timespec ts;
    ts.tv_sec  = (time_t)(ms / 1000u);
    ts.tv_nsec = (long)((ms % 1000u) * 1000000L);
    nanosleep(&ts, NULL);
}

/* --------------------------------------------------------------------- */
/*  Interlocked* — atomic operations on volatile long                     */
/* --------------------------------------------------------------------- */

/* WDSP uses:
 *   _InterlockedAnd, InterlockedAnd      -> atomic fetch_and,  returns OLD value
 *   InterlockedBitTestAndSet(ptr, bit)   -> atomic bit set,    returns OLD bit
 *   InterlockedBitTestAndReset(ptr, bit) -> atomic bit clear,  returns OLD bit
 *
 * Note: on Linux `long` is 64-bit but WDSP only uses these fields as 0/1 flags,
 * so the size mismatch vs Win32 is cosmetic. */

static inline long _wdsp_InterlockedAnd(volatile long *addr, long mask) {
    return (long)__atomic_fetch_and(addr, mask, __ATOMIC_SEQ_CST);
}
static inline long _wdsp_InterlockedOr(volatile long *addr, long mask) {
    return (long)__atomic_fetch_or(addr, mask, __ATOMIC_SEQ_CST);
}
static inline long _wdsp_InterlockedIncrement(volatile long *addr) {
    return (long)__atomic_add_fetch(addr, 1, __ATOMIC_SEQ_CST);
}
static inline long _wdsp_InterlockedDecrement(volatile long *addr) {
    return (long)__atomic_sub_fetch(addr, 1, __ATOMIC_SEQ_CST);
}
static inline unsigned char _wdsp_InterlockedBitTestAndSet(volatile long *addr, long bit) {
    long mask = (long)1 << bit;
    long old  = (long)__atomic_fetch_or(addr, mask, __ATOMIC_SEQ_CST);
    return (old & mask) ? 1 : 0;
}
static inline unsigned char _wdsp_InterlockedBitTestAndReset(volatile long *addr, long bit) {
    long mask = (long)1 << bit;
    long old  = (long)__atomic_fetch_and(addr, ~mask, __ATOMIC_SEQ_CST);
    return (old & mask) ? 1 : 0;
}

#define _InterlockedAnd            _wdsp_InterlockedAnd
#define InterlockedAnd             _wdsp_InterlockedAnd
#define _InterlockedOr             _wdsp_InterlockedOr
#define InterlockedOr              _wdsp_InterlockedOr
#define InterlockedBitTestAndSet   _wdsp_InterlockedBitTestAndSet
#define InterlockedBitTestAndReset _wdsp_InterlockedBitTestAndReset
#define InterlockedIncrement       _wdsp_InterlockedIncrement
#define InterlockedDecrement       _wdsp_InterlockedDecrement

/* --------------------------------------------------------------------- */
/*  min / max — Windows.h macros that WDSP uses unconditionally          */
/* --------------------------------------------------------------------- */

#ifndef min
#  define min(a, b) (((a) < (b)) ? (a) : (b))
#endif
#ifndef max
#  define max(a, b) (((a) > (b)) ? (a) : (b))
#endif

/* --------------------------------------------------------------------- */
/*  QueueUserWorkItem — Windows thread pool, stubbed with a detached     */
/*  pthread. Only analyzer.c uses it.                                     */
/* --------------------------------------------------------------------- */

typedef DWORD (* _wdsp_work_fn)(void *);
BOOL _wdsp_queue_user_work_item(_wdsp_work_fn fn, void *ctx, DWORD flags);
static inline BOOL QueueUserWorkItem(_wdsp_work_fn fn, void *ctx, DWORD flags) {
    return _wdsp_queue_user_work_item(fn, ctx, flags);
}

/* --------------------------------------------------------------------- */
/*  AVRT (MM thread characteristics) — stubs                              */
/* --------------------------------------------------------------------- */

/* Used by wdspmain() in main.c: AvSetMmThreadCharacteristics sets a privileged
 * "Pro Audio" scheduling class on Windows. We return a sentinel non-NULL handle
 * so the upstream code thinks it succeeded; the Revert form accepts it. */
static inline HANDLE AvSetMmThreadCharacteristics(const char *task, DWORD *task_index) {
    (void)task; if (task_index) *task_index = 0;
    return (HANDLE)(uintptr_t)1;
}
static inline BOOL AvSetMmThreadPriority(HANDLE h, int prio) { (void)h; (void)prio; return TRUE; }
static inline BOOL AvRevertMmThreadCharacteristics(HANDLE h) { (void)h; return TRUE; }

/* --------------------------------------------------------------------- */
/*  SSE flush-to-zero macro                                                */
/* --------------------------------------------------------------------- */

#if !defined(__x86_64__) && !defined(__i386__)
#  ifndef _MM_FLUSH_ZERO_ON
#    define _MM_FLUSH_ZERO_ON 0
#  endif
#  ifndef _MM_SET_FLUSH_ZERO_MODE
#    define _MM_SET_FLUSH_ZERO_MODE(x) ((void)(x))
#  endif
#endif

#ifdef __cplusplus
}
#endif

#endif /* WDSP_POSIX_H */
