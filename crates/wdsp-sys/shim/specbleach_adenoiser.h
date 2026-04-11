/* Stub for libspecbleach — phase A compiles WDSP with NR4 disabled.
 * We still need the type name SpectralBleachHandle (used in sbnr struct fields)
 * to resolve; WDSP_NO_SPECBLEACH is honored inside sbnr.c to bypass any actual
 * calls. The real header is restored in phase E. */
#ifndef WDSP_SHIM_SPECBLEACH_STUB_H
#define WDSP_SHIM_SPECBLEACH_STUB_H

typedef void* SpectralBleachHandle;

typedef struct {
    float reduction_amount;
    float smoothing_factor;
    float whitening_factor;
    int   noise_scaling_type;
    float noise_rescale;
    float post_filter_threshold;
} SpectralBleachParameters;

#endif
