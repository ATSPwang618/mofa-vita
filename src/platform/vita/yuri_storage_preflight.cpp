#include "tjsCommHead.h"

#include "StorageIntf.h"
#include "StorageImpl.h"
#include "ScriptMgnIntf.h"
#include "SysInitImpl.h"
#include "DebugIntf.h"
#include "MsgIntf.h"
#include "mofa/retail_bootstrap.hpp"
#include "mofa/system_app_id_compat.hpp"
#include "mofa/vita_executable_name.hpp"
#include "mofa/vita_storage_path.hpp"
#include "mofa/yuri_storage_preflight.hpp"

#include <psp2/io/stat.h>

#include <algorithm>
#include <memory>
#include <string>
#include <vector>

// Yuri implements this media-dispatch helper but omits it from StorageIntf.h.
void TVPGetListAt(const ttstr& name, iTVPStorageLister* lister);

namespace {

class ProjectStorageLister final : public iTVPStorageLister {
public:
    void TJS_INTF_METHOD Add(const ttstr& file) override {
        files.push_back(file);
    }

    std::vector<ttstr> files;
};

void require_openable(const ttstr& path, const char* marker) {
    std::unique_ptr<tTJSBinaryStream> stream(TVPCreateStream(path, TJS_BS_READ));
    if (!stream) TVPThrowExceptionMessage(TJS_W("Cannot open Vita storage"), path);
    mofa_boot_trace(marker);
}

bool has_suffix(const ttstr& value, const tjs_char* suffix) {
    const tjs_int value_length = value.GetLen();
    const tjs_int suffix_length = TJS_strlen(suffix);
    return value_length >= suffix_length &&
        !TJS_strcmp(value.c_str() + value_length - suffix_length, suffix);
}

// Keep the loose install a first-class layout instead of depending on the
// title's own storages.tjs.  The launcher already knows both directories:
// the staged game root (plugin/, savedata/) and the project it selected for
// the engine (the loose tree itself).
//
// Registration order is the priority order of the storage table, so the KAG
// layer directories go last: a title that ships its layer twice (for example
// sub/ and scenario/) must resolve the framework scripts from sub/.
void register_loose_storage_roots(const ttstr& game_root, const ttstr& project) {
    // The project name arrives in the engine's own form (for example
    // "file://./ux0:data/mofa-vita/game/data/"), so strip the URI prefix and
    // canonicalize before asking the file system anything.
    std::u16string native_project = mofa::vita_storage_to_native_path(
        std::u16string_view(project.c_str(), project.GetLen()));
    if (native_project.empty()) return;
    if (native_project.back() != u'/') native_project.push_back(u'/');

    // Resolve existence with the same syscall the retail fstat module uses.
    // The engine's own storage lookup answers a different question (it asks
    // the media layer), so a launcher that registers search paths through it
    // can silently register nothing at all.
    const auto folder_exists = [](const std::u16string& native) {
        if (native.empty()) return false;
        const std::string narrow = ttstr(native).AsNarrowStdString();
        if (narrow.empty()) return false;
        SceIoStat status{};
        return sceIoGetstat(narrow.c_str(), &status) >= 0 &&
               SCE_S_ISDIR(status.st_mode);
    };
    const auto register_folder = [&](const std::u16string& native) {
        if (!folder_exists(native)) return false;
        TVPAddAutoPath(ttstr(native));
        return true;
    };

    std::u16string native_game = mofa::vita_storage_to_native_path(
        std::u16string_view(game_root.c_str(), game_root.GetLen()));
    if (!native_game.empty() && native_game.back() != u'/') native_game.push_back(u'/');

    // The title's plug-ins are addressed through System.exePath, which on a
    // staged Vita install can resolve to the project directory instead of the
    // game root; register the real directory explicitly.
    register_folder(native_game + u"plugin/");

    // The engine's lister reports regular files only, so the directory list is
    // the layout contract itself: the same names the title's storages.tjs
    // iterates.  Later registrations take priority, which is the order this
    // title's own XP3 branch relies on.
    static const char16_t* const kResourceDirectoryNames[] = {
        u"bg/",   u"bgm/",   u"fg/",       u"image/", u"rule/",   u"sound/",
        u"scenario/", u"others/", u"video/", u"override/", u"tool/",
    };
    std::size_t registered = 0;
    for (const char16_t* name : kResourceDirectoryNames)
        if (register_folder(native_project + name)) ++registered;

    // No system/ root: that directory holds another "initialize.tjs" for a KAG
    // variant this title does not boot, and every system/ path is reachable by
    // name through the project root anyway.  The project's own copies of
    // startup.tjs / initialize.tjs / storages.tjs win, and the KAG layer
    // directory is registered last so its framework scripts beat the resource
    // directories for bare names.
    TVPAddAutoPath(ttstr(native_project));
    if (register_folder(native_project + u"sub/")) ++registered;

    const ttstr executable = mofa_yuri_project_executable_path(game_root);
    TVPAddImportantLog(ttstr(TJS_W("[mofa] loose roots: project=")) +
                       ttstr(native_project.c_str()) +
                       TJS_W(" registered=") + ttstr(static_cast<tjs_int>(registered)) +
                       TJS_W(" exe=") +
                       (executable.IsEmpty() ? ttstr(TJS_W("(none)")) : executable));
    mofa_boot_trace("retail-loose-roots-registered");
}

} // namespace

ttstr mofa_yuri_select_project(const ttstr& native_project_root) {
    const std::u16string directory = mofa::vita_directory_path(
        std::u16string_view(native_project_root.c_str(), native_project_root.GetLen()));
    const ttstr root(directory);

    const ttstr content_data = root + TJS_W("content-data");
    if (TVPCheckExistentLocalFolder(content_data)) {
        mofa_boot_trace("retail-project-content-data-selected");
        return content_data + TJS_W("/");
    }

    const ttstr data_xp3 = root + TJS_W("data.xp3");
    if (TVPCheckExistentLocalFile(data_xp3)) {
        mofa_boot_trace("retail-project-data-xp3-selected");
        return data_xp3;
    }

    const ttstr data_exe = root + TJS_W("data.exe");
    if (TVPCheckExistentLocalFile(data_exe)) {
        mofa_boot_trace("retail-project-data-exe-selected");
        return data_exe;
    }

    const ttstr data_directory = root + TJS_W("data");
    if (TVPCheckExistentLocalFolder(data_directory)) {
        mofa_boot_trace("retail-project-data-directory-selected");
        return data_directory + TJS_W("/");
    }

    mofa_boot_trace("retail-project-root-selected");
    return root;
}

ttstr mofa_yuri_project_executable_path(const ttstr& native_project_root) {
    if (native_project_root.IsEmpty()) return ttstr();
    const std::u16string directory = mofa::vita_directory_path(
        std::u16string_view(native_project_root.c_str(),
                            native_project_root.GetLen()));
    const ttstr root(directory);

    ProjectStorageLister lister;
    try {
        TVPGetListAt(TVPNormalizeStorageName(root), &lister);
    } catch (...) {
        return ttstr();
    }

    std::vector<std::u16string> names;
    names.reserve(lister.files.size());
    for (const auto& file : lister.files)
        names.emplace_back(file.c_str(), file.GetLen());

    const std::u16string executable =
        mofa::vita_select_executable_name(names);
    if (executable.empty()) {
        mofa_boot_trace("retail-project-executable-not-identified");
        return ttstr();
    }
    mofa_boot_trace("retail-project-executable-identified");
    return root + ttstr(executable);
}

void mofa_yuri_storage_preflight(const ttstr& native_project_path) {
    mofa_boot_trace("yuri-storage-preflight-entered");

    const std::u16string native = mofa::vita_directory_path(
        std::u16string_view(native_project_path.c_str(), native_project_path.GetLen()));
    const ttstr project(native);
    ProjectStorageLister lister;
    TVPGetListAt(TVPNormalizeStorageName(project), &lister);
    if (lister.files.empty())
        TVPThrowExceptionMessage(TJS_W("Vita game directory is not enumerable"), project);
    mofa_boot_trace("yuri-project-directory-enumerated");

    // Register the loose install before any title script runs: the engine's
    // own project handle then resolves every resource, framework and plug-in
    // name without the title having to add search paths for this layout.
    register_loose_storage_roots(project, TVPProjectDir);

    auto archive = std::find(lister.files.begin(), lister.files.end(), TJS_W("data.xp3"));
    if (archive == lister.files.end()) {
        archive = std::find_if(lister.files.begin(), lister.files.end(),
            [](const ttstr& file) { return has_suffix(file, TJS_W(".xp3")); });
    }
    if (archive != lister.files.end())
        require_openable(project + *archive, "yuri-project-xp3-opened");

    tTJSVariant option;
    if (TVPGetCommandLine(TJS_W("-xp3filter"), &option))
        require_openable(ttstr(option), "yuri-xp3filter-opened");
    if (TVPGetCommandLine(TJS_W("-krkrpatch"), &option))
        require_openable(ttstr(option), "yuri-patch-opened");

    mofa_boot_trace("yuri-storage-preflight-complete");
}

void mofa_yuri_startup_storage_preflight() {
    mofa_boot_trace("yuri-startup-storage-preflight-entered");
    tTJS* engine = TVPGetScriptEngine();
    if (!engine || !mofa::install_system_app_id_compat(*engine)) {
        TVPThrowExceptionMessage(
            TJS_W("Cannot install System.checkAppId compatibility"));
    }
    mofa_boot_trace("yuri-system-app-id-compat-ready");
    require_openable(TJS_W("startup.tjs"), "yuri-startup-storage-opened");
    mofa_boot_trace("yuri-startup-storage-preflight-complete");
}
