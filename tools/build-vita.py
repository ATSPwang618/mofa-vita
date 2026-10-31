#!/usr/bin/env python3
"""Discover a local VitaSDK (or Windows WSL), then run the Vita build.

--check is read-only. Normal builds/--setup may install the pinned Rust toolchain;
KNIFE_VITA_OFFLINE=1 disables these downloads. Python 3.9+, standard library only.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request

PIN = "nightly-2026-05-28"
ROOT = Path(__file__).resolve().parents[1]
MODES = ("platform", "gxm", "image", "effects", "audio", "pipeline", "text",
         "audio-mixer", "game", "movie", "compressed", "ui-color")
SDK_TOOLS = ("arm-vita-eabi-gcc", "arm-vita-eabi-ar", "arm-vita-eabi-readelf",
             "arm-vita-eabi-nm", "arm-vita-eabi-strip", "vita-elf-create", "vita-make-fself",
             "vita-mksfoex", "vita-pack-vpk")
STUBS = ("SceLibKernel_stub", "SceKernelThreadMgr_stub", "SceIofilemgr_stub",
         "SceProcessmgr_stub", "SceGxm_stub", "SceDisplay_stub", "SceSysmem_stub",
         "SceAudio_stub", "SceAvPlayer_stub", "SceSysmodule_stub", "SceJpeg_stub",
         "ScePower_stub", "SceShaccCg_stub_weak", "SceKernelModulemgr_stub")
VENDOR_LIBS = ("png", "jpeg", "z", "opusfile", "opus", "ogg", "taihen_stub")


def capture(command, env=None, timeout=30):
    try:
        p = subprocess.run(command, env=env, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, timeout=timeout, check=False)
        return p.returncode, p.stdout.decode("utf-8", errors="replace").strip(), p.stderr.decode("utf-8", errors="replace").strip()
    except (OSError, subprocess.TimeoutExpired) as exc:
        return 127, "", str(exc)


def native_env(sdk=None):
    env = os.environ.copy()
    cargo_bin = Path((env.get("CARGO_HOME") or str(Path.home() / ".cargo"))).expanduser() / "bin"
    paths = [str(cargo_bin)]
    if sdk is not None:
        env["VITASDK"] = str(sdk)
        paths.insert(0, str(sdk / "bin"))
    # Retain the caller's complete PATH, including Homebrew and custom SDK tools.
    paths.append(env.get("PATH", ""))
    env["PATH"] = os.pathsep.join(paths)
    return env


def sdk_candidates(explicit):
    if explicit:
        return [Path(explicit).expanduser().resolve()]
    result = []
    if os.environ.get("VITASDK"):
        result.append(Path(os.environ["VITASDK"]).expanduser())
    compiler = shutil.which("arm-vita-eabi-gcc", path=native_env()["PATH"])
    if compiler:
        result.extend((Path(compiler).resolve().parent.parent, Path(compiler).parent.parent))
    result.extend(Path(p).expanduser() for p in
                  ("/opt/vitasdk", "/usr/local/vitasdk", "~/vitasdk",
                   "~/.local/vitasdk", "~/Library/PSV/vitasdk"))
    unique = []
    for path in result:
        path = path.resolve()
        if path not in unique:
            unique.append(path)
    return unique


def inspect_sdk(path):
    if not path.is_dir():
        return {"path": str(path), "missing": ["directory does not exist"]}
    missing = []
    for tool in SDK_TOOLS:
        executable = path / "bin" / tool
        if not executable.is_file() or not os.access(str(executable), os.X_OK):
            missing.append("bin/" + tool)
    for name in ("share/vita.toolchain.cmake", "share/vita.cmake",
                 "arm-vita-eabi/include/psp2/gxm.h", "arm-vita-eabi/include/psp2/shacccg.h",
                 "arm-vita-eabi/lib/crt0.o", "arm-vita-eabi/lib/libc.a", "arm-vita-eabi/lib/libm.a"):
        if not (path / name).is_file():
            missing.append(name)
    for stub in STUBS:
        name = "arm-vita-eabi/lib/lib" + stub + ".a"
        if not (path / name).is_file():
            missing.append(name)
    info = {"path": str(path), "missing": missing}
    if not missing:
        gcc = str(path / "bin/arm-vita-eabi-gcc")
        rc, out, err = capture([gcc, "--version"], native_env(path))
        if rc:
            missing.append("SDK GCC cannot execute on this host: " + (err or out))
        else:
            info["gcc"] = out.splitlines()[0] if out else gcc
            rc, out, err = capture([gcc, "-print-multi-lib"], native_env(path))
            # Query the driver only: no source compilation or generated object.
            info["multilib"] = out if rc == 0 else (err or "unavailable")
            info["hard_float_multilib"] = rc == 0 and "mfloat-abi=hard" in out
            rc, out, err = capture([gcc, "-v"], native_env(path))
            info["hard_float_default"] = rc == 0 and "--with-float=hard" in (out + err)
    return info


def rust_status(env):
    rustup = shutil.which("rustup", path=env["PATH"])
    result = {"rustup": rustup, "toolchain": PIN, "installed": False, "rust_src": False, "missing": []}
    if not rustup:
        result["missing"].append("rustup")
        return result
    rc, out, err = capture([rustup, "toolchain", "list"], env)
    if rc:
        result["missing"].append("rustup toolchain list: " + (err or out))
        return result
    result["installed"] = any(line.split()[0] == PIN or line.split()[0].startswith(PIN + "-")
                              for line in out.splitlines() if line.strip())
    if not result["installed"]:
        result["missing"].append(PIN)
        return result
    rc, out, err = capture([rustup, "run", PIN, "rustc", "--print", "sysroot"], env)
    if rc:
        result["missing"].append("pinned rustc cannot execute: " + (err or out))
        return result
    result["rust_src"] = (Path(out) / "lib/rustlib/src/rust/library/Cargo.toml").is_file()
    if not result["rust_src"]:
        result["missing"].append(PIN + " rust-src")
    if not shutil.which("cargo", path=env["PATH"]):
        result["missing"].append("cargo rustup proxy")
    return result


def inspect_native(args):
    candidates = [inspect_sdk(p) for p in sdk_candidates(args.sdk)]
    sdk = next((c for c in candidates if not c["missing"]), None)
    env = native_env(Path(sdk["path"]) if sdk else None)
    missing = []
    tools = {}
    for name in ("cmake", "ninja", "python3", "bash"):
        value = shutil.which(name, path=env["PATH"])
        tools[name] = value
        if not value:
            missing.append(name)
    for name in ("ninja", "python3", "bash"):
        if tools[name]:
            rc, out, err = capture([tools[name], "--version"], env)
            if rc:
                missing.append(name + " cannot execute: " + (err or out))
    if tools["python3"]:
        rc, out, err = capture([tools["python3"], "-c", "import sys; sys.exit(0 if sys.version_info >= (3,9) else 1)"], env)
        if rc:
            missing.append("python3 >= 3.9")
    if tools["cmake"]:
        rc, out, err = capture([tools["cmake"], "--version"], env)
        match = re.search(r"cmake version (\d+)\.(\d+)", out)
        if rc or not match or tuple(map(int, match.groups())) < (3, 20):
            missing.append("cmake >= 3.20 (" + (out.splitlines()[0] if out else err) + ")")
    for lib in VENDOR_LIBS:
        file = ROOT / "knife/vita/vendor/dependencies/lib" / ("lib" + lib + ".a")
        if not file.is_file():
            missing.append(str(file.relative_to(ROOT)))
    for header in ("png.h", "jpeglib.h", "zlib.h", "opus/opusfile.h", "ogg/ogg.h", "taihen.h"):
        file = ROOT / "knife/vita/vendor/dependencies/include" / header
        if not file.is_file():
            missing.append(str(file.relative_to(ROOT)))
    for relative in ("knife/vita/vendor/shacccg-ext/libSceShaccCgExt.a",
                     "knife/vita/vendor/shacccg-ext/LICENSE", "knife/vita/vendor/shacccg-ext/NOTICE",
                     "knife/vita/shaders/generated/shaders.h", "knife/vita/shaders/generated/effects.h",
                     "knife/vita/shaders/generated/runtime_source.h", "tools/build-vita.sh",
                     "tools/sanitize-vita-elf.py"):
        if not (ROOT / relative).is_file():
            missing.append(relative)
    # These are build inputs, not files to look up in another local project.
    compiler = ROOT / "knife/vita/vendor/shacccg/libshacccg.suprx"
    if not compiler.is_file() or hashlib.sha256(compiler.read_bytes()).hexdigest() != "0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552":
        missing.append("bundled libshacccg.suprx missing or modified; restore vendor files from Git")
    manifest = ROOT / "knife/vita/vendor/dependencies/manifest.json"
    try:
        for row in json.loads(manifest.read_text())["files"]:
            file = manifest.parent / row["path"]
            if not file.is_file() or hashlib.sha256(file.read_bytes()).hexdigest() != row["sha256"]:
                missing.append("bundled dependency missing or modified: " + row["path"])
    except (OSError, ValueError, KeyError) as exc:
        missing.append("bundled dependency manifest: " + str(exc))
    if sdk is None:
        missing.insert(0, "compatible VitaSDK (see candidates)")
    rust = rust_status(env)
    return {"host": platform.system(), "sdk": sdk["path"] if sdk else None,
            "candidates": candidates, "tools": tools, "base_missing": missing,
            "rust": rust, "ready": not missing and not rust["missing"]}, env


def show_report(report):
    print("Vita build host: " + report["host"])
    for candidate in report["candidates"]:
        print("SDK " + candidate["path"] + (": ready" if not candidate["missing"] else ": unavailable"))
        if candidate["missing"]:
            print("  Missing: " + ", ".join(candidate["missing"]))
        elif candidate.get("gcc"):
            print("  " + candidate["gcc"])
            abi = "hard-float default" if candidate.get("hard_float_default") else ("hard-float multilib" if candidate.get("hard_float_multilib") else "driver metadata does not identify default float ABI")
            print("  SDK ABI: " + abi + " (driver query only)")
    for name, value in report["tools"].items():
        print(name + ": " + (value or "MISSING"))
    print("Rust " + PIN + " + rust-src: " + ("ready" if not report["rust"]["missing"] else ", ".join(report["rust"]["missing"])))
    if report["base_missing"]:
        print("Missing prerequisites: " + "; ".join(report["base_missing"]))
    print("Check: " + ("ready" if report["ready"] else "incomplete"))


def setup_rust(env):
    status = rust_status(env)
    if not status["missing"]:
        return
    if env.get("KNIFE_VITA_OFFLINE") == "1":
        raise RuntimeError("Rust setup needs downloads but KNIFE_VITA_OFFLINE=1: " + ", ".join(status["missing"]))
    if not status["rustup"]:
        bash = shutil.which("bash", path=env["PATH"])
        if not bash:
            raise RuntimeError("bash is required to install rustup")
        print("Installing rustup over HTTPS (no default toolchain, no PATH/profile edits).", flush=True)
        with tempfile.TemporaryDirectory(prefix="knife-vita-rustup-") as directory:
            script = Path(directory) / "rustup-init.sh"
            with urllib.request.urlopen("https://sh.rustup.rs", timeout=60) as response:
                script.write_bytes(response.read())
            subprocess.run([bash, str(script), "-y", "--default-toolchain", "none", "--no-modify-path"], env=env, check=True)
        status = rust_status(env)
    rustup = status["rustup"]
    if not rustup:
        raise RuntimeError("rustup installation did not create a usable cargo/bin/rustup")
    if not status["installed"]:
        subprocess.run([rustup, "toolchain", "install", PIN, "--profile", "minimal", "--component", "rust-src"], env=env, check=True)
    elif not status["rust_src"]:
        subprocess.run([rustup, "component", "add", "rust-src", "--toolchain", PIN], env=env, check=True)
    status = rust_status(env)
    if status["missing"]:
        raise RuntimeError("Rust setup incomplete: " + ", ".join(status["missing"]))


def decode_wsl(data):
    # wsl.exe -l -q is UTF-16LE on several supported Windows releases.
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        return data.decode("utf-16", errors="replace").strip()
    return data.decode("utf-16-le" if b"\0" in data else "utf-8", errors="replace").strip()


def wsl_path(wsl, distro, path):
    rc, out, err = capture([wsl, "-d", distro, "--", "wslpath", "-u", str(path)])
    if rc or not out:
        raise RuntimeError("WSL path conversion failed in " + distro + ": " + err)
    return out.splitlines()[-1]


def wsl_prefix(wsl, distro):
    prefix = [wsl, "-d", distro, "--", "env"]
    # WSL does not automatically import arbitrary Windows environment variables.
    # Carry build preferences/offline mode without replacing its Linux PATH/HOME.
    for key, value in os.environ.items():
        if key == "KNIFE_VITA_SHADER_COMPILER":
            continue  # The build uses the checked-in module on every host.
        if key.startswith("KNIFE_"):
            if key == "KNIFE_VITA_CACHE" and re.match(r"^[A-Za-z]:[\\/]", value):
                value = wsl_path(wsl, distro, value)
            prefix.append(key + "=" + value)
    return prefix


def windows_main(args):
    wsl = shutil.which("wsl.exe")
    if not wsl:
        raise RuntimeError("wsl.exe was not found. Install WSL and a Linux distribution, or run this launcher on Linux/macOS.")
    if args.wsl:
        distros = [args.wsl]
    else:
        p = subprocess.run([wsl, "--list", "--quiet"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
        if p.returncode:
            raise RuntimeError("Cannot list WSL distributions: " + decode_wsl(p.stderr))
        distros = [line.strip().lstrip("*").strip() for line in decode_wsl(p.stdout).splitlines() if line.strip()]
    if not distros:
        raise RuntimeError("No WSL distributions found; specify --wsl DISTRO after installing one.")
    choices = []
    for distro in distros:
        try:
            script = wsl_path(wsl, distro, Path(__file__).resolve())
            forwarded = ["--sdk", wsl_path(wsl, distro, args.sdk)] if args.sdk and re.match(r"^[A-Za-z]:[\\/]", args.sdk) else (["--sdk", args.sdk] if args.sdk else [])
            probe = wsl_prefix(wsl, distro) + ["python3", script, "--check", "--native", "--json"] + forwarded
            rc, out, err = capture(probe, timeout=90)
            try:
                report = json.loads(out)
            except ValueError:
                print("WSL " + distro + ": " + (err or out or "Python/check unavailable"))
                continue
            print("WSL " + distro + ":")
            show_report(report)
            choices.append((rc, report, distro, script, forwarded))
        except RuntimeError as exc:
            print(str(exc))
    ready = next((choice for choice in choices if choice[0] == 0), None)
    # If Rust alone is absent, normal builds may bootstrap it in that distro.
    # --check never enters this path and never changes the distribution.
    selected = ready or (next((c for c in choices if not c[1]["base_missing"]), None) if not args.check else None)
    if args.setup and not selected:
        selected = next((c for c in choices if c[1]["tools"].get("bash")), None)
    if not selected:
        return 1
    if args.check:
        print("Selected WSL: " + selected[2])
        return 0
    _, _, distro, script, forwarded = selected
    command = wsl_prefix(wsl, distro) + ["python3", script, "--native"] + forwarded
    command += ["--setup"] if args.setup else [args.mode, args.build_kind]
    print("Selected WSL: " + distro, flush=True)
    return subprocess.call(command)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", nargs="?", default="game", choices=MODES)
    parser.add_argument("build_kind", nargs="?", default="release", choices=("dev", "release"))
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument("--check", action="store_true", help="read-only environment discovery; do not install or build")
    actions.add_argument("--setup", action="store_true", help="only install missing pinned Rust/rust-src; do not build")
    parser.add_argument("--sdk", help="explicit VitaSDK directory (automatic discovery otherwise)")
    parser.add_argument("--native", action="store_true", help="use this Linux/macOS host directly; never dispatch WSL")
    parser.add_argument("--wsl", metavar="DISTRO", help="select a Windows WSL distribution")
    parser.add_argument("--json", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if sys.version_info < (3, 9):
        parser.error("Python 3.9 or newer is required")
    if not args.check and not args.setup and args.build_kind == "release" and args.mode != "game":
        parser.error("release is available for game; use MODE dev for probes")
    if platform.system() == "Windows":
        if args.native:
            parser.error("native builds require Linux/macOS; omit --native to discover WSL")
        return windows_main(args)
    if args.wsl:
        parser.error("--wsl is a Windows option; use --native or --sdk here")
    if platform.system() not in ("Linux", "Darwin"):
        parser.error("supported native hosts are Linux and macOS")
    if args.setup:
        setup_rust(native_env())
        print("Rust " + PIN + " + rust-src: ready")
        return 0
    report, env = inspect_native(args)
    if args.json:
        print(json.dumps(report, ensure_ascii=False))
    else:
        show_report(report)
    if args.check:
        return 0 if report["ready"] else 1
    if report["base_missing"]:
        return 1
    setup_rust(env)
    env["RUSTUP_TOOLCHAIN"] = PIN
    command = [report["tools"]["bash"], str(ROOT / "tools/build-vita.sh"), args.mode, args.build_kind]
    return subprocess.call(command, cwd=str(ROOT), env=env)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RuntimeError, OSError, subprocess.CalledProcessError) as exc:
        print("Vita build: " + str(exc), file=sys.stderr)
        sys.exit(1)
