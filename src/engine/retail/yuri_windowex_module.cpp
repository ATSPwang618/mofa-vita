#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// windowEx.dll extends Window/MenuItem/Pad/Debug.console/Scripts/System with
// the Win32 window and multi-monitor surface (Window.setZoom, window rects,
// System.getDisplayMonitors, Scripts.eval, ...).  KAG's window override links
// it unconditionally while booting.  Vita has a single fixed full-screen
// surface, so register the module for the link and let the platform provide
// the window geometry it does have; a title that needs a Win32-only entry
// point must stay a visible compatibility gap.
#define NCB_MODULE_NAME TJS_W("windowEx.dll")

namespace {

void mark_windowex_load_only() {
    mofa_boot_trace("retail-windowex-load-only-ready");
}

} // namespace

NCB_POST_REGIST_CALLBACK(mark_windowex_load_only);
