#include "ncbind/ncbind.hpp"

#include "mofa/retail_bootstrap.hpp"

// KAGEX titles link a long tail of Windows-only plug-ins by name.  None of
// them can be loaded on Vita, and several have no portable implementation
// available (see README's plug-in table).  Registering the published names
// keeps Plugins.link() from ending the boot, which is what the compatibility
// model calls "link-only": the name resolves, the behaviour does not exist.
//
// Every entry below stays classified as link-only by
// mofa::yuri_plugin_is_link_only(), so a title that actually reaches one of
// these entry points is still reported as a compatibility gap instead of a
// working implementation.  Titles that need a real behaviour get a dedicated
// module (see yuri_layerexareaaverage_module.cpp and the layerEx* modules).
//
// - stringUtil.dll: isNumber / parseKeyFrame / initSpline helpers used by
//   KAGEX key-frame actions.  Currently only the link is provided.
// - Equations.dll: the interpolation set; the compat patch supplies a TJS
//   `class Equations`, so the link is all this build owes the script.
// - json.dll: referenced by TimeLinePlugin.tjs; the engine's own
//   Scripts.evalJSON covers the parsing that is actually called.
// - snow3d.dll / messenger.dll / SystemExTouchImage.dll: optional effect and
//   debug helpers that the titles link from guarded branches.
// - base64.dll / qrcode.dll / registory.dll / win32ole.dll: Windows-only
//   helpers (OLE automation, registry access) used by the crash-report and
//   updater scripts.
namespace {

void trace_stringutil() {
    mofa_boot_trace("retail-stringutil-load-only-ready");
}
void trace_equations() {
    mofa_boot_trace("retail-equations-load-only-ready");
}
void trace_json() {
    mofa_boot_trace("retail-json-load-only-ready");
}
void trace_snow3d() {
    mofa_boot_trace("retail-snow3d-load-only-ready");
}
void trace_messenger() {
    mofa_boot_trace("retail-messenger-load-only-ready");
}
void trace_systemextouchimage() {
    mofa_boot_trace("retail-systemextouchimage-load-only-ready");
}
void trace_base64() {
    mofa_boot_trace("retail-base64-load-only-ready");
}
void trace_qrcode() {
    mofa_boot_trace("retail-qrcode-load-only-ready");
}
void trace_registory() {
    mofa_boot_trace("retail-registory-load-only-ready");
}
void trace_win32ole() {
    mofa_boot_trace("retail-win32ole-load-only-ready");
}

// ncbAutoRegister folds the name to lower case when it builds the registry, so
// the published spelling here matches what Plugins.link() requests.
ncbCallbackAutoRegister register_stringutil(
    TJS_W("stringUtil.dll"), ncbAutoRegister::PostRegist, &trace_stringutil, 0);
ncbCallbackAutoRegister register_equations(
    TJS_W("Equations.dll"), ncbAutoRegister::PostRegist, &trace_equations, 0);
ncbCallbackAutoRegister register_json(
    TJS_W("json.dll"), ncbAutoRegister::PostRegist, &trace_json, 0);
ncbCallbackAutoRegister register_snow3d(
    TJS_W("snow3d.dll"), ncbAutoRegister::PostRegist, &trace_snow3d, 0);
ncbCallbackAutoRegister register_messenger(
    TJS_W("messenger.dll"), ncbAutoRegister::PostRegist, &trace_messenger, 0);
ncbCallbackAutoRegister register_systemextouchimage(
    TJS_W("SystemExTouchImage.dll"), ncbAutoRegister::PostRegist,
    &trace_systemextouchimage, 0);
ncbCallbackAutoRegister register_base64(
    TJS_W("base64.dll"), ncbAutoRegister::PostRegist, &trace_base64, 0);
ncbCallbackAutoRegister register_qrcode(
    TJS_W("qrcode.dll"), ncbAutoRegister::PostRegist, &trace_qrcode, 0);
ncbCallbackAutoRegister register_registory(
    TJS_W("registory.dll"), ncbAutoRegister::PostRegist, &trace_registory, 0);
ncbCallbackAutoRegister register_win32ole(
    TJS_W("win32ole.dll"), ncbAutoRegister::PostRegist, &trace_win32ole, 0);

} // namespace
