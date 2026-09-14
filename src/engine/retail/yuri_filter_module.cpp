#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// filter.dll is the closed-source KiriKiri Z image filter
// (doHaze / monoCro / noise / contrast, ...).  KAGEX titles link it next to
// layerExFilter.dll and map the contrast path onto the engine's own light
// primitive from their compat patch.  Register the module so the link
// succeeds; unfiltered output must stay a visible compatibility gap.
#define NCB_MODULE_NAME TJS_W("filter.dll")

namespace {

void mark_filter_load_only() {
    mofa_boot_trace("retail-filter-load-only-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(mark_filter_load_only);
