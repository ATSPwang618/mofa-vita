// Windows-only diagnostic launcher. Debugs only the process it creates, preserves
// its real exit code, and never changes registry, heap flags, or guest state.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <dbghelp.h>
#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <map>
#include <sstream>
#include <string>
#include <vector>

static std::string utf8(const std::wstring &s) {
    if (s.empty()) return {};
    int n = WideCharToMultiByte(CP_UTF8, 0, s.data(), int(s.size()), nullptr, 0, nullptr, nullptr);
    std::string out(n, '\0');
    WideCharToMultiByte(CP_UTF8, 0, s.data(), int(s.size()), out.data(), n, nullptr, nullptr);
    return out;
}
static std::string json(const std::string &s) {
    std::ostringstream out;
    out << '"';
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') out << '\\' << c;
        else if (c < 32) out << "\\u" << std::hex << std::setfill('0') << std::setw(4) << unsigned(c) << std::dec;
        else out << c;
    }
    return out.str() + '"';
}
static std::string hex(uint64_t v) {
    std::ostringstream out; out << "0x" << std::hex << v; return out.str();
}
static std::wstring quote(const std::wstring &s) {
    std::wstring out = L"\"";
    size_t slashes = 0;
    for (wchar_t c : s) {
        if (c == L'\\') { ++slashes; continue; }
        out.append(c == L'"' ? slashes * 2 + 1 : slashes, L'\\');
        out += c; slashes = 0;
    }
    out.append(slashes * 2, L'\\');
    return out + L'"';
}
static std::wstring handle_path(HANDLE h) {
    if (!h || h == INVALID_HANDLE_VALUE) return {};
    std::wstring out(32768, L'\0');
    DWORD n = GetFinalPathNameByHandleW(h, out.data(), DWORD(out.size()), FILE_NAME_NORMALIZED);
    if (!n || n >= out.size()) return {};
    out.resize(n); return out;
}
// Use the target's actual PE exception tables for x64 unwinding. DbgHelp's
// symbol-only table lookup can omit unwind entries when no PDB is available.
struct RuntimeFunction { DWORD begin, end, unwind; };
static_assert(sizeof(RuntimeFunction) == 12);
static std::map<DWORD64, std::vector<RuntimeFunction>> unwind_tables;
static bool read_remote(HANDLE process, DWORD64 address, void *data, size_t size) {
    SIZE_T got = 0;
    return ReadProcessMemory(process, reinterpret_cast<LPCVOID>(address), data, size, &got) && got == size;
}
static DWORD64 CALLBACK image_base(HANDLE process, DWORD64 pc) {
    MEMORY_BASIC_INFORMATION info{};
    if (VirtualQueryEx(process, reinterpret_cast<LPCVOID>(pc), &info, sizeof(info)) && info.Type == MEM_IMAGE)
        return reinterpret_cast<DWORD64>(info.AllocationBase);
    return SymGetModuleBase64(process, pc);
}
static PVOID CALLBACK function_table(HANDLE process, DWORD64 pc) {
    auto base = image_base(process, pc);
    if (!base || pc < base || pc - base > UINT32_MAX) return SymFunctionTableAccess64(process, pc);
    auto found = unwind_tables.find(base);
    if (found == unwind_tables.end()) {
        std::vector<RuntimeFunction> table;
        IMAGE_DOS_HEADER dos{}; IMAGE_NT_HEADERS64 nt{};
        if (read_remote(process, base, &dos, sizeof(dos)) && dos.e_magic == IMAGE_DOS_SIGNATURE
            && dos.e_lfanew > 0 && dos.e_lfanew <= 1024 * 1024
            && read_remote(process, base + dos.e_lfanew, &nt, sizeof(nt))
            && nt.Signature == IMAGE_NT_SIGNATURE && nt.FileHeader.Machine == IMAGE_FILE_MACHINE_AMD64
            && nt.OptionalHeader.Magic == IMAGE_NT_OPTIONAL_HDR64_MAGIC) {
            auto directory = nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXCEPTION];
            if (directory.VirtualAddress && directory.Size && directory.Size <= 64 * 1024 * 1024
                && directory.Size % sizeof(RuntimeFunction) == 0
                && uint64_t(directory.VirtualAddress) + directory.Size <= nt.OptionalHeader.SizeOfImage) {
                table.resize(directory.Size / sizeof(RuntimeFunction));
                if (!read_remote(process, base + directory.VirtualAddress, table.data(), directory.Size)) table.clear();
            }
        }
        found = unwind_tables.emplace(base, std::move(table)).first;
    }
    auto &table = found->second;
    const DWORD rva = DWORD(pc - base);
    auto entry = std::upper_bound(table.begin(), table.end(), rva,
        [](DWORD address, const RuntimeFunction &value) { return address < value.begin; });
    if (entry != table.begin()) {
        --entry;
        if (entry->begin <= rva && rva < entry->end) return &*entry;
    }
    return SymFunctionTableAccess64(process, pc);
}
static void frame(std::ostream &out, HANDLE process, DWORD64 pc,
                  const std::map<DWORD64, std::wstring> &modules) {
    out << "{\"pc\":" << json(hex(pc));
    MEMORY_BASIC_INFORMATION info{};
    if (VirtualQueryEx(process, reinterpret_cast<LPCVOID>(pc), &info, sizeof(info))) {
        auto base = reinterpret_cast<DWORD64>(info.AllocationBase);
        auto found = modules.find(base);
        if (found != modules.end()) out << ",\"module\":" << json(utf8(found->second))
            << ",\"module_base\":" << json(hex(base)) << ",\"rva\":" << json(hex(pc - base));
    }
    alignas(SYMBOL_INFO) unsigned char storage[sizeof(SYMBOL_INFO) + MAX_SYM_NAME]{};
    auto symbol = reinterpret_cast<SYMBOL_INFO *>(storage);
    symbol->SizeOfStruct = sizeof(SYMBOL_INFO); symbol->MaxNameLen = MAX_SYM_NAME;
    DWORD64 displacement = 0;
    if (SymFromAddr(process, pc, &displacement, symbol)) out << ",\"symbol\":"
        << json(std::string(symbol->Name, symbol->NameLen)) << ",\"symbol_displacement\":" << json(hex(displacement));
    out << '}';
}

int wmain(int argc, wchar_t **argv) {
    if (argc < 4 || std::wstring(argv[2]) != L"--") {
        fputs("Usage: vita3k-owned-debugger.exe ABSENT_OUTPUT_DIRECTORY -- EXECUTABLE [arguments]\n", stderr);
        return 2;
    }
    std::filesystem::path output(argv[1]);
    if (!output.is_absolute() || !std::filesystem::is_directory(output.parent_path()) || std::filesystem::exists(output)) return 2;
    if (!std::filesystem::create_directory(output)) return 2;
    std::ofstream log(output / L"events.jsonl", std::ios::binary);
    if (!log) return 2;
    auto emit = [&](const std::string &line) { log << line << '\n'; log.flush(); };
    std::wstring command;
    for (int i = 3; i < argc; ++i) { if (i > 3) command += L' '; command += quote(argv[i]); }
    STARTUPINFOW startup{}; startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
    startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION child{};
    if (!CreateProcessW(argv[3], command.data(), nullptr, nullptr, TRUE,
                        DEBUG_ONLY_THIS_PROCESS | CREATE_UNICODE_ENVIRONMENT, nullptr, nullptr, &startup, &child)) {
        emit("{\"kind\":\"launch_failed\",\"win32_error\":" + std::to_string(GetLastError()) + "}"); return 2;
    }
    // Both handles from CREATE_PROCESS_DEBUG_EVENT belong to the debug loop.
    CloseHandle(child.hThread);
    FILETIME created{}, exited{}, kernel{}, user{};
    GetProcessTimes(child.hProcess, &created, &exited, &kernel, &user);
    uint64_t created_ticks = (uint64_t(created.dwHighDateTime) << 32) | created.dwLowDateTime;
    emit("{\"kind\":\"owned_process\",\"pid\":" + std::to_string(child.dwProcessId)
        + ",\"creation_filetime\":" + json(hex(created_ticks)) + ",\"executable\":" + json(utf8(argv[3]))
        + ",\"debug_only_this_process\":true,\"process_state_changes\":\"debug-event suspension and continuation only\"}");
    SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_FAIL_CRITICAL_ERRORS | SYMOPT_NO_PROMPTS);
    // Empty search path: no symbol-server requests or inherited symbol path.
    const bool symbols = SymInitialize(child.hProcess, "", FALSE) != FALSE;
    std::map<DWORD, HANDLE> threads;
    std::map<DWORD64, std::wstring> modules;
    DWORD result = 2; bool done = false, expired = false, initial_break_seen = false;
    unsigned dumps = 0;
    const auto start = GetTickCount64();
    while (!done) {
        DEBUG_EVENT event{};
        if (!WaitForDebugEvent(&event, 1000)) {
            if (GetLastError() != ERROR_SEM_TIMEOUT) {
                emit("{\"kind\":\"debug_wait_failed\",\"win32_error\":" + std::to_string(GetLastError()) + "}"); break;
            }
            if (!expired && GetTickCount64() - start > 300000) {
                expired = true;
                emit("{\"kind\":\"owned_process_timeout\",\"timeout_ms\":300000}");
                TerminateProcess(child.hProcess, 0xCFFFFFFE);
            }
            continue;
        }
        DWORD disposition = DBG_CONTINUE;
        if (event.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT || event.dwDebugEventCode == LOAD_DLL_DEBUG_EVENT) {
            const bool initial = event.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT;
            HANDLE file = initial ? event.u.CreateProcessInfo.hFile : event.u.LoadDll.hFile;
            DWORD64 base = reinterpret_cast<DWORD64>(initial ? event.u.CreateProcessInfo.lpBaseOfImage : event.u.LoadDll.lpBaseOfDll);
            auto filename = handle_path(file);
            if (initial) {
                threads[event.dwThreadId] = event.u.CreateProcessInfo.hThread;
                if (event.u.CreateProcessInfo.hProcess != child.hProcess) CloseHandle(event.u.CreateProcessInfo.hProcess);
                if (filename.empty()) filename = argv[3];
            }
            modules[base] = filename;
            SetLastError(0);
            auto symbol_base = symbols ? SymLoadModuleExW(child.hProcess, file, filename.c_str(), nullptr, base, 0, nullptr, 0) : 0;
            auto symbol_error = GetLastError();
            emit("{\"kind\":\"load_module\",\"base\":" + json(hex(base)) + ",\"path\":" + json(utf8(filename))
                + ",\"symbols_loaded\":" + (symbol_base ? "true" : "false") + ",\"symbol_error\":" + std::to_string(symbol_error) + "}");
            if (file) CloseHandle(file);
        } else if (event.dwDebugEventCode == UNLOAD_DLL_DEBUG_EVENT) {
            auto base = reinterpret_cast<DWORD64>(event.u.UnloadDll.lpBaseOfDll);
            if (symbols) SymUnloadModule64(child.hProcess, base);
            modules.erase(base);
            unwind_tables.erase(base);
        } else if (event.dwDebugEventCode == CREATE_THREAD_DEBUG_EVENT) {
            threads[event.dwThreadId] = event.u.CreateThread.hThread;
        } else if (event.dwDebugEventCode == EXIT_THREAD_DEBUG_EVENT) {
            auto found = threads.find(event.dwThreadId);
            if (found != threads.end()) { CloseHandle(found->second); threads.erase(found); }
        } else if (event.dwDebugEventCode == EXCEPTION_DEBUG_EVENT) {
            auto &exception = event.u.Exception;
            const DWORD code = exception.ExceptionRecord.ExceptionCode;
            // Continue only the debugger's initial break. All target exceptions
            // retain normal application/OS handling; the launcher never fixes one.
            const bool initial_break = code == EXCEPTION_BREAKPOINT && exception.dwFirstChance && !initial_break_seen;
            if (initial_break) initial_break_seen = true;
            disposition = initial_break ? DBG_CONTINUE : DBG_EXCEPTION_NOT_HANDLED;
            std::ostringstream observation;
            observation << "{\"kind\":\"exception\",\"thread\":" << event.dwThreadId
                << ",\"code\":" << json(hex(code)) << ",\"address\":"
                << json(hex(reinterpret_cast<uint64_t>(exception.ExceptionRecord.ExceptionAddress)))
                << ",\"first_chance\":" << (exception.dwFirstChance ? "true" : "false")
                << ",\"debugger_initial_break\":" << (initial_break ? "true" : "false");
            // First-chance access violations can be handled by the application.
            // Capture fatal heap/fail-fast events and unhandled exceptions only.
            const bool fatal = code == 0xC0000374 || code == 0xC0000409 || !exception.dwFirstChance;
            if (fatal) {
                auto thread = threads.find(event.dwThreadId);
                CONTEXT context{}; context.ContextFlags = CONTEXT_FULL;
                const bool got = thread != threads.end() && GetThreadContext(thread->second, &context);
                observation << ",\"context_captured\":" << (got ? "true" : "false");
                if (got) {
                    observation << ",\"rsp\":" << json(hex(context.Rsp)) << ",\"rbp\":" << json(hex(context.Rbp)) << ",\"stack\":[";
                    CONTEXT walking = context; STACKFRAME64 stack{};
                    stack.AddrPC.Offset = context.Rip; stack.AddrPC.Mode = AddrModeFlat;
                    stack.AddrFrame.Offset = context.Rbp; stack.AddrFrame.Mode = AddrModeFlat;
                    stack.AddrStack.Offset = context.Rsp; stack.AddrStack.Mode = AddrModeFlat;
                    frame(observation, child.hProcess, context.Rip, modules);
                    DWORD64 previous = context.Rip;
                    for (unsigned i = 0; symbols && i < 63; ++i) {
                        if (!StackWalk64(IMAGE_FILE_MACHINE_AMD64, child.hProcess, thread->second, &stack,
                                         &walking, nullptr, function_table, image_base, nullptr) || !stack.AddrPC.Offset) break;
                        if (previous == stack.AddrPC.Offset) continue;
                        observation << ','; frame(observation, child.hProcess, stack.AddrPC.Offset, modules); previous = stack.AddrPC.Offset;
                    }
                    observation << ']';
                }
                if (dumps < 2) {
                    auto name = L"exception-" + std::to_wstring(++dumps) + L".dmp";
                    auto path = output / name;
                    HANDLE file = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
                    EXCEPTION_POINTERS pointers{&exception.ExceptionRecord, got ? &context : nullptr};
                    MINIDUMP_EXCEPTION_INFORMATION info{event.dwThreadId, &pointers, FALSE};
                    const auto flags = MINIDUMP_TYPE(MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules | MiniDumpWithProcessThreadData | MiniDumpWithIndirectlyReferencedMemory);
                    BOOL written = file != INVALID_HANDLE_VALUE && MiniDumpWriteDump(child.hProcess, child.dwProcessId, file, flags, got ? &info : nullptr, nullptr, nullptr);
                    DWORD error = written ? 0 : GetLastError();
                    if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
                    observation << ",\"dump\":" << json(utf8(path.wstring())) << ",\"dump_written\":"
                        << (written ? "true" : "false") << ",\"dump_error\":" << error;
                }
            }
            observation << '}'; emit(observation.str());
        } else if (event.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT) {
            result = event.u.ExitProcess.dwExitCode;
            emit("{\"kind\":\"exit\",\"code\":" + std::to_string(result) + ",\"elapsed_ms\":" + std::to_string(GetTickCount64() - start) + "}"); done = true;
        }
        if (!ContinueDebugEvent(event.dwProcessId, event.dwThreadId, disposition)) {
            emit("{\"kind\":\"continue_failed\",\"win32_error\":" + std::to_string(GetLastError()) + "}"); break;
        }
    }
    if (symbols) SymCleanup(child.hProcess);
    for (auto [id, handle] : threads) CloseHandle(handle);
    CloseHandle(child.hProcess);
    log.close();
    // ExitProcess preserves unsigned NTSTATUS, including 0xC0000374.
    ExitProcess(result);
}
