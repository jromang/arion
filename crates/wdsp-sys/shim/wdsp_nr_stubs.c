/* wdsp_nr_stubs.c — no-op fallbacks for NR3 (RNNoise) and NR4
 * (libspecbleach) when the corresponding system library is not
 * installed.
 *
 * Each half is guarded by its own `WDSP_NO_*` define (set by the
 * build script when `pkg-config` can't find the lib). When the real
 * lib IS present, the build script:
 *   1. undefines `WDSP_NO_RNNOISE` / `WDSP_NO_SPECBLEACH`,
 *   2. adds upstream's `rnnr.c` / `sbnr.c` to the source list,
 *   3. adds the lib's `pkg-config --libs` entry.
 *
 * In that case this file compiles empty for the matching half, so
 * the linker pulls in the real `create_rnnr` / `create_sbnr` from
 * `rnnr.c` / `sbnr.c` rather than the stubs below.
 *
 * The behaviour of the stubs themselves is:
 *   - `create_*` returns a small heap-allocated block whose first
 *     field (`int run`) reads as zero, because upstream WDSP code
 *     dereferences `rxa[channel].rnnr.p->run` from several places
 *     and would segfault on NULL.
 *   - every other entry point is a no-op.
 */

#include <stdlib.h>

#ifdef WDSP_NO_RNNOISE
typedef void* RNNR;
#endif
#ifdef WDSP_NO_SPECBLEACH
typedef void* SBNR;
#endif

/* Shared layout: both rnnr and sbnr start with `int run` at offset 0,
 * which is all upstream WDSP reads from outside the module. We hand
 * out a 64-byte calloc'd block so any stray read of a later field
 * still lands in zeroed memory rather than unmapped pages. */
#if defined(WDSP_NO_RNNOISE) || defined(WDSP_NO_SPECBLEACH)
typedef struct {
    int run;
    unsigned char _pad[60];
} _wdsp_nr_stub;
#endif

/* ---- RNNR (NR3) ----------------------------------------------------- */
#ifdef WDSP_NO_RNNOISE

RNNR create_rnnr(int run, int position, int size, double *in, double *out, int rate) {
    (void)run; (void)position; (void)size; (void)in; (void)out; (void)rate;
    return calloc(1, sizeof(_wdsp_nr_stub));
}
void destroy_rnnr(RNNR a)                             { free(a); }
void xrnnr(RNNR a, int pos)                           { (void)a; (void)pos; }
void setSize_rnnr(RNNR a, int size)                   { (void)a; (void)size; }
void setBuffers_rnnr(RNNR a, double *in, double *out) { (void)a; (void)in; (void)out; }
void setSamplerate_rnnr(RNNR a, int rate)             { (void)a; (void)rate; }

#endif /* WDSP_NO_RNNOISE */

/* ---- SBNR (NR4) ----------------------------------------------------- */
#ifdef WDSP_NO_SPECBLEACH

SBNR create_sbnr(int run, int position, int size, double *in, double *out, int rate) {
    (void)run; (void)position; (void)size; (void)in; (void)out; (void)rate;
    return calloc(1, sizeof(_wdsp_nr_stub));
}
void destroy_sbnr(SBNR a)                             { free(a); }
void xsbnr(SBNR a, int pos)                           { (void)a; (void)pos; }
void setSize_sbnr(SBNR a, int size)                   { (void)a; (void)size; }
void setBuffers_sbnr(SBNR a, double *in, double *out) { (void)a; (void)in; (void)out; }
void setSamplerate_sbnr(SBNR a, int rate)             { (void)a; (void)rate; }

#endif /* WDSP_NO_SPECBLEACH */
