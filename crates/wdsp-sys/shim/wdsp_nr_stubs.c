/* wdsp_nr_stubs.c — phase A stub implementations for the NR3 (RNNoise) and
 * NR4 (libspecbleach) noise-reduction modules.
 *
 * The real implementations live in rnnr.c / sbnr.c and depend on two
 * third-party C libraries (rnnoise, libspecbleach). Rather than vendoring
 * those for phase A, we exclude rnnr.c and sbnr.c from the cc build and
 * provide no-op shims here so that the upstream call sites in RXA.c link
 * successfully.
 *
 * Behaviour: create_* returns NULL, every other entry point is a no-op. The
 * rest of the RX chain tolerates NULL rnnr/sbnr pointers because RXA.c only
 * calls into them when the corresponding run flag is set (and that flag is
 * zero-initialised). If that assumption ever fails we'll see a segfault and
 * the phase E work to bring in real RNNoise/SpecBleach begins.
 */

#include <stdlib.h>

/* Match the typedefs from the stub headers. */
typedef void* RNNR;
typedef void* SBNR;

/* Upstream call sites in RXA.c / amd.c / anr.c / anf.c / emnr.c / snb.c
 * dereference `rxa[channel].rnnr.p->run` and `rxa[channel].sbnr.p->run` even
 * when the module is inactive. So `create_*` must return a non-NULL pointer
 * to a struct whose first field `int run` reads as zero. We hand out a small
 * calloc'd block — both rnnr and sbnr start with `int run` at offset 0. */
typedef struct {
    int run;
    /* Pad up to 64 bytes so any stray reads past `run` land in zeroed memory
     * rather than unmapped pages. Upstream code paths that live inside the
     * excluded rnnr.c / sbnr.c translation units would need the real layout,
     * but those are compiled out for phase A. */
    unsigned char _pad[60];
} _wdsp_nr_stub;

/* ---- RNNR (NR3) ----------------------------------------------------- */

RNNR create_rnnr(int run, int position, int size, double *in, double *out, int rate) {
    (void)run; (void)position; (void)size; (void)in; (void)out; (void)rate;
    return calloc(1, sizeof(_wdsp_nr_stub));
}
void destroy_rnnr(RNNR a)                             { free(a); }
void xrnnr(RNNR a, int pos)                           { (void)a; (void)pos; }
void setSize_rnnr(RNNR a, int size)                   { (void)a; (void)size; }
void setBuffers_rnnr(RNNR a, double *in, double *out) { (void)a; (void)in; (void)out; }
void setSamplerate_rnnr(RNNR a, int rate)             { (void)a; (void)rate; }

/* ---- SBNR (NR4) ----------------------------------------------------- */

SBNR create_sbnr(int run, int position, int size, double *in, double *out, int rate) {
    (void)run; (void)position; (void)size; (void)in; (void)out; (void)rate;
    return calloc(1, sizeof(_wdsp_nr_stub));
}
void destroy_sbnr(SBNR a)                             { free(a); }
void xsbnr(SBNR a, int pos)                           { (void)a; (void)pos; }
void setSize_sbnr(SBNR a, int size)                   { (void)a; (void)size; }
void setBuffers_sbnr(SBNR a, double *in, double *out) { (void)a; (void)in; (void)out; }
void setSamplerate_sbnr(SBNR a, int rate)             { (void)a; (void)rate; }
