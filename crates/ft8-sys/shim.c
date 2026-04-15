/* Expose sizes / offsets that are otherwise private to ft8_lib, plus
 * re-implement demo/gen_ft8.c's synth_gfsk verbatim here so the Rust
 * side doesn't have to mirror the GFSK arithmetic. */
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include "common/monitor.h"

/* mingw's libc doesn't ship stpcpy (GNU/POSIX extension) but
 * ft8_lib's message.c calls it unconditionally. Provide the tiny
 * replacement here when building for Windows. */
#if defined(_WIN32) || defined(__MINGW32__) || defined(__MINGW64__)
char *stpcpy(char *dst, const char *src) {
    size_t n = strlen(src);
    memcpy(dst, src, n + 1);
    return dst + n;
}
#endif

size_t arion_ft8_monitor_sizeof(void) { return sizeof(monitor_t); }

const ftx_waterfall_t* arion_ft8_monitor_waterfall(const monitor_t* me) {
    return &me->wf;
}

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* π · √(2 / ln 2) — same constant as demo/gen_ft8.c */
#define ARION_GFSK_CONST_K 5.336446f

static void arion_gfsk_pulse(int n_spsym, float symbol_bt, float* pulse) {
    for (int i = 0; i < 3 * n_spsym; ++i) {
        float t = (float)i / (float)n_spsym - 1.5f;
        float a1 = ARION_GFSK_CONST_K * symbol_bt * (t + 0.5f);
        float a2 = ARION_GFSK_CONST_K * symbol_bt * (t - 0.5f);
        pulse[i] = (erff(a1) - erff(a2)) / 2.0f;
    }
}

/* Synthesize a GFSK signal from a tone sequence, writing exactly
 * n_sym * n_spsym samples into `signal`. Behaviour is byte-identical
 * to synth_gfsk() in ft8_lib's demo/gen_ft8.c, so any Rust caller is
 * guaranteed to produce the same wire as the reference encoder. */
void arion_ft8_synth_gfsk(const uint8_t* symbols,
                          int n_sym,
                          float f0,
                          float symbol_bt,
                          float symbol_period,
                          int signal_rate,
                          float* pulse_scratch,   /* [3 * n_spsym] */
                          float* dphi_scratch,    /* [n_wave + 2 * n_spsym] */
                          float* signal)
{
    int n_spsym = (int)(0.5f + signal_rate * symbol_period);
    int n_wave = n_sym * n_spsym;
    float hmod = 1.0f;
    float dphi_peak = 2.0f * (float)M_PI * hmod / (float)n_spsym;

    for (int i = 0; i < n_wave + 2 * n_spsym; ++i) {
        dphi_scratch[i] = 2.0f * (float)M_PI * f0 / (float)signal_rate;
    }

    arion_gfsk_pulse(n_spsym, symbol_bt, pulse_scratch);

    for (int i = 0; i < n_sym; ++i) {
        int ib = i * n_spsym;
        for (int j = 0; j < 3 * n_spsym; ++j) {
            dphi_scratch[j + ib] += dphi_peak * (float)symbols[i] * pulse_scratch[j];
        }
    }

    for (int j = 0; j < 2 * n_spsym; ++j) {
        dphi_scratch[j] +=
            dphi_peak * pulse_scratch[j + n_spsym] * (float)symbols[0];
        dphi_scratch[j + n_sym * n_spsym] +=
            dphi_peak * pulse_scratch[j] * (float)symbols[n_sym - 1];
    }

    float phi = 0.0f;
    for (int k = 0; k < n_wave; ++k) {
        signal[k] = sinf(phi);
        phi = fmodf(phi + dphi_scratch[k + n_spsym], 2.0f * (float)M_PI);
    }

    int n_ramp = n_spsym / 8;
    for (int i = 0; i < n_ramp; ++i) {
        float env = (1.0f - cosf(2.0f * (float)M_PI * (float)i / (2.0f * (float)n_ramp))) / 2.0f;
        signal[i] *= env;
        signal[n_wave - 1 - i] *= env;
    }
}
