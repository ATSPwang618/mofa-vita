#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// clipboardEx.dll extends KiriKiri's own Clipboard class so KAGEX titles can
// copy a screenshot as a bitmap (Clipboard.setAsBitmap) and query text formats
// (Clipboard.hasFormat).  KAG's window override links it unconditionally while
// booting, but Vita has no system clipboard: the titles only reach the bitmap
// path through a debug key chord.  Register the module so the unconditional
// link succeeds; clipboard writes stay unavailable and must remain a visible
// compatibility gap.
#define NCB_MODULE_NAME TJS_W("clipboardEx.dll")

namespace {

void mark_clipboardex_load_only() {
    mofa_boot_trace("retail-clipboardex-load-only-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(mark_clipboardex_load_only);
