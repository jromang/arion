/* wdsp_posix.c — POSIX side of the WDSP shim.
 *
 * Everything that isn't naturally inline lives here: critical section init,
 * HANDLE allocation, semaphore/event wrappers, and the _beginthread trampoline.
 *
 * HANDLE is implemented as a pointer to a heap-allocated `struct _wdsp_handle`
 * tagged with a kind enum. WDSP never introspects handles — it only passes
 * them back to Wait/Release/Close — so a custom representation is safe.
 */
#include "wdsp_posix.h"

#include <assert.h>
#include <stdlib.h>

void _wdsp_cs_init(CRITICAL_SECTION *cs) {
    pthread_mutexattr_t attr;
    pthread_mutexattr_init(&attr);
    pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&cs->mtx, &attr);
    pthread_mutexattr_destroy(&attr);
    cs->initialized = 1;
}

void _wdsp_cs_delete(CRITICAL_SECTION *cs) {
    if (cs->initialized) {
        pthread_mutex_destroy(&cs->mtx);
        cs->initialized = 0;
    }
}

/* ------------------------------------------------------------------ */
/*  Handle representation                                              */
/* ------------------------------------------------------------------ */

typedef enum {
    WDSP_H_SEM = 1,
    WDSP_H_EVENT,
} wdsp_handle_kind;

struct _wdsp_handle {
    wdsp_handle_kind kind;
    /* Semaphore: we use POSIX sem_t directly. */
    sem_t           sem;
    /* Event fields (manual/auto reset events). Implemented with a mutex/cond
     * because POSIX doesn't have a one-shot-auto-reset primitive. */
    int             manual_reset;
    int             signaled;
    pthread_mutex_t ev_mtx;
    pthread_cond_t  ev_cond;
};

HANDLE _wdsp_sem_create(long initial_count, long max_count) {
    (void)max_count; /* POSIX sem_t has no maximum; WDSP tolerates that */
    struct _wdsp_handle *h = calloc(1, sizeof(*h));
    if (!h) return NULL;
    h->kind = WDSP_H_SEM;
    if (sem_init(&h->sem, 0, (unsigned)initial_count) != 0) {
        free(h);
        return NULL;
    }
    return (HANDLE)h;
}

HANDLE _wdsp_event_create(BOOL manual_reset, BOOL initial_state) {
    struct _wdsp_handle *h = calloc(1, sizeof(*h));
    if (!h) return NULL;
    h->kind         = WDSP_H_EVENT;
    h->manual_reset = manual_reset ? 1 : 0;
    h->signaled     = initial_state ? 1 : 0;
    pthread_mutex_init(&h->ev_mtx, NULL);
    pthread_cond_init(&h->ev_cond, NULL);
    return (HANDLE)h;
}

DWORD _wdsp_wait_single(HANDLE handle, DWORD timeout_ms) {
    struct _wdsp_handle *h = (struct _wdsp_handle *)handle;
    if (!h) return WAIT_FAILED;

    if (h->kind == WDSP_H_SEM) {
        if (timeout_ms == INFINITE) {
            while (sem_wait(&h->sem) != 0) {
                if (errno != EINTR) return WAIT_FAILED;
            }
            return WAIT_OBJECT_0;
        }
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        ts.tv_sec  += (time_t)(timeout_ms / 1000u);
        ts.tv_nsec += (long)((timeout_ms % 1000u) * 1000000L);
        if (ts.tv_nsec >= 1000000000L) { ts.tv_sec++; ts.tv_nsec -= 1000000000L; }
        while (sem_timedwait(&h->sem, &ts) != 0) {
            if (errno == ETIMEDOUT) return WAIT_TIMEOUT;
            if (errno != EINTR)     return WAIT_FAILED;
        }
        return WAIT_OBJECT_0;
    }

    if (h->kind == WDSP_H_EVENT) {
        pthread_mutex_lock(&h->ev_mtx);
        if (timeout_ms == INFINITE) {
            while (!h->signaled) {
                pthread_cond_wait(&h->ev_cond, &h->ev_mtx);
            }
        } else {
            struct timespec ts;
            clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_sec  += (time_t)(timeout_ms / 1000u);
            ts.tv_nsec += (long)((timeout_ms % 1000u) * 1000000L);
            if (ts.tv_nsec >= 1000000000L) { ts.tv_sec++; ts.tv_nsec -= 1000000000L; }
            while (!h->signaled) {
                int rc = pthread_cond_timedwait(&h->ev_cond, &h->ev_mtx, &ts);
                if (rc == ETIMEDOUT) {
                    pthread_mutex_unlock(&h->ev_mtx);
                    return WAIT_TIMEOUT;
                }
            }
        }
        if (!h->manual_reset) h->signaled = 0;
        pthread_mutex_unlock(&h->ev_mtx);
        return WAIT_OBJECT_0;
    }

    return WAIT_FAILED;
}

BOOL _wdsp_sem_release(HANDLE handle, long release_count, long *prev_count) {
    struct _wdsp_handle *h = (struct _wdsp_handle *)handle;
    if (!h || h->kind != WDSP_H_SEM) return FALSE;
    if (prev_count) {
        int v = 0;
        sem_getvalue(&h->sem, &v);
        *prev_count = (long)v;
    }
    for (long i = 0; i < release_count; ++i) {
        if (sem_post(&h->sem) != 0) return FALSE;
    }
    return TRUE;
}

BOOL _wdsp_event_set(HANDLE handle) {
    struct _wdsp_handle *h = (struct _wdsp_handle *)handle;
    if (!h || h->kind != WDSP_H_EVENT) return FALSE;
    pthread_mutex_lock(&h->ev_mtx);
    h->signaled = 1;
    if (h->manual_reset) pthread_cond_broadcast(&h->ev_cond);
    else                 pthread_cond_signal(&h->ev_cond);
    pthread_mutex_unlock(&h->ev_mtx);
    return TRUE;
}

BOOL _wdsp_event_reset(HANDLE handle) {
    struct _wdsp_handle *h = (struct _wdsp_handle *)handle;
    if (!h || h->kind != WDSP_H_EVENT) return FALSE;
    pthread_mutex_lock(&h->ev_mtx);
    h->signaled = 0;
    pthread_mutex_unlock(&h->ev_mtx);
    return TRUE;
}

BOOL _wdsp_close_handle(HANDLE handle) {
    struct _wdsp_handle *h = (struct _wdsp_handle *)handle;
    if (!h) return FALSE;
    if (h->kind == WDSP_H_SEM) {
        sem_destroy(&h->sem);
    } else if (h->kind == WDSP_H_EVENT) {
        pthread_mutex_destroy(&h->ev_mtx);
        pthread_cond_destroy(&h->ev_cond);
    }
    free(h);
    return TRUE;
}

/* ------------------------------------------------------------------ */
/*  _beginthread                                                       */
/* ------------------------------------------------------------------ */

struct _wdsp_thread_arg {
    void (*fn)(void *);
    void  *arg;
};

static void *_wdsp_thread_trampoline(void *raw) {
    struct _wdsp_thread_arg *a = (struct _wdsp_thread_arg *)raw;
    void (*fn)(void *) = a->fn;
    void *arg          = a->arg;
    free(a);
    fn(arg);
    return NULL;
}

/* ------------------------------------------------------------------ */
/*  QueueUserWorkItem                                                   */
/* ------------------------------------------------------------------ */

struct _wdsp_work_arg {
    DWORD (*fn)(void *);
    void   *ctx;
};

static void *_wdsp_work_trampoline(void *raw) {
    struct _wdsp_work_arg *a = (struct _wdsp_work_arg *)raw;
    DWORD (*fn)(void *) = a->fn;
    void   *ctx         = a->ctx;
    free(a);
    (void)fn(ctx);
    return NULL;
}

BOOL _wdsp_queue_user_work_item(DWORD (*fn)(void *), void *ctx, DWORD flags) {
    (void)flags;
    struct _wdsp_work_arg *a = (struct _wdsp_work_arg *)malloc(sizeof(*a));
    if (!a) return FALSE;
    a->fn = fn; a->ctx = ctx;
    pthread_t tid;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    int rc = pthread_create(&tid, &attr, _wdsp_work_trampoline, a);
    pthread_attr_destroy(&attr);
    if (rc != 0) { free(a); return FALSE; }
    return TRUE;
}

uintptr_t _wdsp_beginthread(void (*start)(void *), unsigned stack_size, void *arg) {
    struct _wdsp_thread_arg *a = (struct _wdsp_thread_arg *)malloc(sizeof(*a));
    if (!a) return 0;
    a->fn  = start;
    a->arg = arg;

    pthread_attr_t attr;
    pthread_attr_init(&attr);
    if (stack_size > 0 && stack_size >= 16384u /* PTHREAD_STACK_MIN-ish */) {
        pthread_attr_setstacksize(&attr, (size_t)stack_size);
    }
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);

    pthread_t tid;
    int rc = pthread_create(&tid, &attr, _wdsp_thread_trampoline, a);
    pthread_attr_destroy(&attr);
    if (rc != 0) {
        free(a);
        return 0;
    }
    return (uintptr_t)tid;
}
