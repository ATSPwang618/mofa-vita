#include "tjsCommHead.h"

#include "StorageImpl.h"
#include "StorageIntf.h"
#include "tjsArray.h"
#include "ncbind/ncbind.hpp"
#include "mofa/retail_bootstrap.hpp"
#include "mofa/vita_storage_path.hpp"

#include <memory>
#include <string>
#include <string_view>
#include <cstdint>
#include <vector>

#include <psp2/io/fcntl.h>
#include <psp2/io/stat.h>

#define NCB_MODULE_NAME TJS_W("fstat.dll")

namespace {

tjs_error fstat_dirlist(tTJSVariant* result, tjs_int numparams,
                        tTJSVariant** param, iTJSDispatch2*) {
    if (numparams < 1) return TJS_E_BADPARAMCOUNT;

    ttstr directory(*param[0]);
    if (directory.GetLastChar() != TJS_W('/')) {
        TVPThrowExceptionMessage(
            TJS_W("'/' must be specified at the end of given directory name."));
    }
    directory = TVPNormalizeStorageName(directory);

    iTJSDispatch2* array = TJSCreateArrayObject();
    if (!result) {
        array->Release();
        return TJS_S_OK;
    }
    try {
        tTJSArrayNI* native = nullptr;
        const auto status = array->NativeInstanceSupport(
            TJS_NIS_GETINSTANCE, TJSGetArrayClassID(),
            reinterpret_cast<iTJSNativeInstance**>(&native));
        if (TJS_FAILED(status) || !native) {
            array->Release();
            return status;
        }
        TVPGetLocalName(directory);
        TVPGetLocalFileListAt(
            directory, [native](const ttstr& name, tTVPLocalFileInfo* info) {
                if (info && (info->Mode & (S_IFREG | S_IFDIR)))
                    native->Items.emplace_back(name);
            });
        *result = tTJSVariant(array, array);
        array->Release();
    } catch (...) {
        array->Release();
        throw;
    }
    return TJS_S_OK;
}

// KiriKiri Z's fstat.dll also supplies the file/directory helpers that Z-era
// startup scripts link before use:
//
//     Plugins.link("fstat.dll");
//     if(Storages.isExistentDirectory(System.dataPath)) { ... }
//     Storages.createDirectory(patchdir);
//     if(Storages.isExistentImage(storage)) { ... }
//     Storages.deleteFile(name); Storages.copyFile(from, to, false);
//
// Yuri is a KiriKiri2-era engine and never had these members, so a Z title
// stops during boot with `Member "isExistentDirectory" does not exist` before
// KAG runs a single game script.  The wrappers below install the same
// contract on Yuri's Storages object, resolving storage names (including the
// "file://./ux0:data/..." form System.dataPath hands out) through the shared
// Vita canonical path helper.

std::u16string as_u16(const ttstr& value) {
    return std::u16string(value.c_str(),
                          static_cast<std::size_t>(value.GetLen()));
}

// Vita file APIs take UTF-8 narrow paths, so resolve the storage name to the
// canonical native path first and hand sceIo* the same narrow form the
// storage bootstrap uses.
std::string as_narrow(const std::u16string& path) {
    return ttstr(path).AsNarrowStdString();
}

std::u16string native_storage_path(tTJSVariant* value) {
    if (!value) return std::u16string();
    const ttstr name(*value);
    return mofa::vita_storage_to_native_path(
        std::u16string_view(name.c_str(),
                            static_cast<std::size_t>(name.GetLen())));
}

bool local_folder_exists(const std::string& path) {
    if (path.empty()) return false;
    SceIoStat status{};
    return sceIoGetstat(path.c_str(), &status) >= 0 &&
           SCE_S_ISDIR(status.st_mode);
}

bool local_file_exists(const std::string& path) {
    if (path.empty()) return false;
    SceIoStat status{};
    return sceIoGetstat(path.c_str(), &status) >= 0 &&
           SCE_S_ISREG(status.st_mode);
}

void set_boolean(tTJSVariant* result, bool value) {
    if (result) *result = static_cast<tjs_int>(value ? 1 : 0);
}

tjs_error fstat_is_existent_directory(tTJSVariant* result, tjs_int numparams,
                                      tTJSVariant** param, iTJSDispatch2*) {
    if (numparams < 1) return TJS_E_BADPARAMCOUNT;
    set_boolean(result,
                local_folder_exists(as_narrow(native_storage_path(param[0]))));
    return TJS_S_OK;
}

tjs_error fstat_create_directory(tTJSVariant* result, tjs_int numparams,
                                 tTJSVariant** param, iTJSDispatch2*) {
    if (numparams < 1) return TJS_E_BADPARAMCOUNT;
    const std::u16string wide_path = native_storage_path(param[0]);
    const std::string path = as_narrow(wide_path);
    if (path.empty()) {
        set_boolean(result, false);
        return TJS_S_OK;
    }
    // Z scripts call this with System.dataPath-relative directories whose
    // parents normally exist, but creating every component keeps a partially
    // populated memory card working as well.  The device prefix ("ux0:") is
    // ASCII, so its UTF-16 offset is valid in the UTF-8 form too, and it is
    // never passed to sceIoMkdir.
    const std::size_t device =
        mofa::vita_device_prefix_length(wide_path);
    for (std::size_t index = 0; index <= path.size(); ++index) {
        if (index != path.size() && path[index] != '/') continue;
        if (index <= device) continue;
        sceIoMkdir(path.substr(0, index).c_str(), 0777);
    }
    set_boolean(result, local_folder_exists(path));
    return TJS_S_OK;
}

tjs_error fstat_is_existent_image(tTJSVariant* result, tjs_int numparams,
                                  tTJSVariant** param, iTJSDispatch2*) {
    if (numparams < 1) return TJS_E_BADPARAMCOUNT;
    // KAG's own fallback (compositionlayer.tjs) defines this as "the same
    // storage with any of the engine's image extensions exists", so mirror
    // that contract instead of guessing at the file headers.
    const ttstr storage(*param[0]);
    std::u16string base = as_u16(storage);
    const std::size_t separator = base.find_last_of(u"/\\");
    const std::size_t dot = base.find_last_of(u'.');
    if (dot != std::u16string::npos &&
        (separator == std::u16string::npos || dot > separator)) {
        base.erase(dot);
    }
    static const tjs_char* const extensions[] = {
        TJS_W(".tlg"), TJS_W(".png"), TJS_W(".bmp"),
        TJS_W(".jpg"), TJS_W(".jpeg"),
    };
    for (const tjs_char* extension : extensions) {
        if (TVPIsExistentStorage(ttstr(base + extension))) {
            set_boolean(result, true);
            return TJS_S_OK;
        }
    }
    set_boolean(result, false);
    return TJS_S_OK;
}

tjs_error fstat_delete_file(tTJSVariant* result, tjs_int numparams,
                            tTJSVariant** param, iTJSDispatch2*) {
    if (numparams < 1) return TJS_E_BADPARAMCOUNT;
    ttstr local(*param[0]);
    TVPGetLocalName(local);
    const std::string path =
        local.IsEmpty() ? std::string() : local.AsNarrowStdString();
    set_boolean(result, !path.empty() && sceIoRemove(path.c_str()) >= 0);
    return TJS_S_OK;
}

tjs_error fstat_copy_file(tTJSVariant* result, tjs_int numparams,
                          tTJSVariant** param, iTJSDispatch2*) {
    if (numparams < 2) return TJS_E_BADPARAMCOUNT;
    const bool overwrite =
        numparams >= 3 && static_cast<tjs_int>(*param[2]) != 0;
    ttstr from(*param[0]);
    ttstr to(*param[1]);
    TVPGetLocalName(from);
    TVPGetLocalName(to);
    if (from.IsEmpty() || to.IsEmpty()) {
        set_boolean(result, false);
        return TJS_S_OK;
    }
    const std::string destination = to.AsNarrowStdString();
    // The third argument follows KiriKiri Z's "overwrite" flag: when a caller
    // asks not to overwrite an existing file the copy is a no-op that still
    // reports success, which is what the save-data rotation path expects.
    if (!overwrite && local_file_exists(destination)) {
        set_boolean(result, true);
        return TJS_S_OK;
    }
    std::unique_ptr<tTJSBinaryStream> input(
        TVPCreateStream(from, TJS_BS_READ));
    if (!input) {
        set_boolean(result, false);
        return TJS_S_OK;
    }
    const int output = sceIoOpen(destination.c_str(),
        SCE_O_WRONLY | SCE_O_CREAT | SCE_O_TRUNC, 0666);
    if (output < 0) {
        set_boolean(result, false);
        return TJS_S_OK;
    }
    bool okay = true;
    std::vector<std::uint8_t> buffer(64 * 1024);
    for (;;) {
        const tjs_uint read = input->Read(
            buffer.data(), static_cast<tjs_uint>(buffer.size()));
        if (read == 0) break;
        tjs_uint written = 0;
        while (written < read) {
            const int step = sceIoWrite(
                output, buffer.data() + written, read - written);
            if (step <= 0) {
                okay = false;
                break;
            }
            written += static_cast<tjs_uint>(step);
        }
        if (!okay) break;
    }
    if (okay && sceIoSyncByFd(output, 0) < 0) okay = false;
    sceIoClose(output);
    if (!okay) sceIoRemove(destination.c_str());
    set_boolean(result, okay);
    return TJS_S_OK;
}

void mark_fstat_ready() {
    mofa_boot_trace("retail-fstat-ready");
}

} // namespace

NCB_ATTACH_FUNCTION(dirlist, Storages, fstat_dirlist);
NCB_ATTACH_FUNCTION(isExistentDirectory, Storages, fstat_is_existent_directory);
NCB_ATTACH_FUNCTION(createDirectory, Storages, fstat_create_directory);
NCB_ATTACH_FUNCTION(isExistentImage, Storages, fstat_is_existent_image);
NCB_ATTACH_FUNCTION(deleteFile, Storages, fstat_delete_file);
NCB_ATTACH_FUNCTION(copyFile, Storages, fstat_copy_file);
NCB_POST_REGIST_CALLBACK(mark_fstat_ready);
