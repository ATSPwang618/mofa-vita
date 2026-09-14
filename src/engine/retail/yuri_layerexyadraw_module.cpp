#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// layerExYaDraw.dll is the closed-source focus-line/drawing plug-in
// (drawFocusLines plus its clip* helpers).  FocusLinePlugin.tjs links it when
// a scenario enables the focus-line effect, and the companion compat patch
// supplies a TJS replacement for the drawing entry point.  Register the module
// so the link succeeds; the original drawing kernel is not reproduced here.
#define NCB_MODULE_NAME TJS_W("layerExYaDraw.dll")

namespace {

void mark_layerexyadraw_load_only() {
    mofa_boot_trace("retail-layerexyadraw-load-only-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(mark_layerexyadraw_load_only);
