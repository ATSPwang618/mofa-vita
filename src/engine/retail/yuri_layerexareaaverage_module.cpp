#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// The layerExAreaAverage surface itself comes from the upstream wamsoft
// implementation fetched from the pinned KrKr2-Next source. That module
// attaches the real Layer.stretchCopyAA area-average kernel, so this
// translation unit only records that the plug-in reached registration for the
// hardware boot log. Titles such as KAGEX-derived KAG window overrides link it
// outside a try/catch, so an absent module ends the boot rather than costing
// an optional effect.
#define NCB_MODULE_NAME TJS_W("layerExAreaAverage.dll")

namespace {

void trace_layerexareaaverage_ready() {
    mofa_boot_trace("retail-layerexareaaverage-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(trace_layerexareaaverage_ready);
