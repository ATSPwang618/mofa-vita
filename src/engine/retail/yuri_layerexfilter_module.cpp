#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// layerExFilter.dll is a closed-source wamsoft filter plug-in
// (drawNoise / doContrast / initHazeCopy / hazeCopy / realImage*).  KiriKiriZ
// titles link it while KAG loads its system scripts, and the compat patch
// published alongside those titles replaces the filter entry points with TJS
// implementations of its own.  Register the module so those links succeed;
// a title that reaches a filter entry point without such a patch must remain
// an explicit compatibility gap rather than silently claiming a filter.
#define NCB_MODULE_NAME TJS_W("layerExFilter.dll")

namespace {

void mark_layerexfilter_load_only() {
    mofa_boot_trace("retail-layerexfilter-load-only-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(mark_layerexfilter_load_only);
