#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// layerExParticle.dll drives per-layer particles (initVectorParticle,
// initRotateParticle, assignParticle, updateParticle, particleImage, ...).
// KAGEX-derived titles link it from AnimationLayer.override.tjs, and the
// companion compat patch replaces those entry points with TJS stubs. Register
// the module so the link succeeds; particle playback itself is not
// implemented here and must stay a visible compatibility gap.
#define NCB_MODULE_NAME TJS_W("layerExParticle.dll")

namespace {

void mark_layerexparticle_load_only() {
    mofa_boot_trace("retail-layerexparticle-load-only-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(mark_layerexparticle_load_only);
