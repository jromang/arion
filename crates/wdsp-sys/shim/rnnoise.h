/* Stub for rnnoise.h — phase A compiles WDSP with NR3 disabled.
 * We only need the opaque DenoiseState type name to resolve; any call into
 * rnnoise is gated by WDSP_NO_RNNOISE inside rnnr.c. The real header is
 * restored in phase E. */
#ifndef WDSP_SHIM_RNNOISE_STUB_H
#define WDSP_SHIM_RNNOISE_STUB_H

typedef struct DenoiseState DenoiseState;
typedef struct RNNModel     RNNModel;

#endif
