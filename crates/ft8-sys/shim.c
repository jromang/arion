/* Expose sizes / offsets that are otherwise private to ft8_lib so
 * the Rust side can allocate opaque storage correctly. */
#include <stddef.h>
#include "common/monitor.h"

size_t arion_ft8_monitor_sizeof(void) { return sizeof(monitor_t); }

const ftx_waterfall_t* arion_ft8_monitor_waterfall(const monitor_t* me) {
    return &me->wf;
}
