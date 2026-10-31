use std::{env, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    let status = command.status().expect("start native build tool");
    assert!(status.success(), "native build failed: {command:?}");
}

fn main() {
    for name in ["perf_audit.c", "perf_audit.h", "perf_counters.def"] {
        println!("cargo:rerun-if-changed=../vita/{name}");
    }
    let definitions = std::fs::read_to_string("../vita/perf_counters.def").expect("performance metric definitions");
    let names = definitions.lines().filter(|l| l.starts_with("KV_PERF("))
        .map(|l| l.split('"').nth(1).expect("metric label")).collect::<Vec<_>>();
    let profile_path = if env::var_os("CARGO_FEATURE_ASSETS_540P").is_some() {
        "src/asset_profile_540p.rs"
    } else { "src/asset_profile_270p.rs" };
    println!("cargo:rerun-if-changed={profile_path}");
    let profile = std::fs::read_to_string(profile_path).expect("asset profile");
    let manifest = profile.lines().find_map(|line| line.strip_prefix("// External manifest SHA-256: ")).unwrap_or("unknown");
    std::fs::write(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("perf_names.rs"),
        format!("pub const NATIVE_NAMES: &[&str] = &{:?};\npub const ASSET_MANIFEST: &str = {:?};\n", names, manifest)).unwrap();
    println!("cargo:rerun-if-env-changed=VITASDK");
    println!("cargo:rerun-if-env-changed=HOME");
    println!("cargo:rerun-if-env-changed=KNIFE_VITA_DEV_DIAGNOSTICS");
    println!("cargo:rerun-if-env-changed=KNIFE_VITA_QUIET");
    println!("cargo:rerun-if-changed=../vita/diagnostic_log.h");
    println!("cargo:rerun-if-changed=../vita/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../vita/platform.c");
    println!("cargo:rerun-if-changed=../vita/platform.h");
    println!("cargo:rerun-if-changed=../vita/gxm_backend.c");
    println!("cargo:rerun-if-changed=../vita/gxm_backend.h");
    println!("cargo:rerun-if-changed=../vita/effects_backend.inc");
    println!("cargo:rerun-if-changed=../vita/gpu_lifetime.inc");
    for name in ["runtime_shaders.c", "runtime_shaders.h", "shader_sha256.h", "shader_module_exports.h", "shader_extensions.c", "shader_extensions.h", "vendor/shacccg-ext/libSceShaccCgExt.a", "shaders/effects.cg", "shaders/generated/runtime_source.h"] {
        println!("cargo:rerun-if-changed=../vita/{name}");
    }
    println!("cargo:rerun-if-changed=../vita/shaders/generated/effects.h");
    println!("cargo:rerun-if-changed=../vita/vendor/shacccg-ext/NOTICE");
    println!("cargo:rerun-if-changed=../vita/vendor/dependencies");
    println!("cargo:rerun-if-changed=../../tools/generate-runtime-shaders.py");
    println!("cargo:rerun-if-changed=../vita/audio_decode.c");
    println!("cargo:rerun-if-changed=../vita/audio_decode.h");
    println!("cargo:rerun-if-changed=../vita/audio_output.c");
    println!("cargo:rerun-if-changed=../vita/audio_output.h");
    println!("cargo:rerun-if-changed=../vita/image_decode.c");
    println!("cargo:rerun-if-changed=../vita/image_decode.h");
    println!("cargo:rerun-if-changed=../vita/jpeg_hw.c");
    println!("cargo:rerun-if-changed=../vita/jpeg_hw.h");
    println!("cargo:rerun-if-changed=../vita/jpeg_hw_driver.h");
    println!("cargo:rerun-if-changed=../vita/movie_player.c");
    println!("cargo:rerun-if-changed=../vita/movie_player.h");
    println!("cargo:rerun-if-changed=../vita/shaders/generated/shaders.h");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("vita") {
        return;
    }
    let sdk = env::var("VITASDK").expect("set VITASDK to the installed SDK root");
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../vita");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native");
    let diagnostics = if env::var("KNIFE_VITA_DEV_DIAGNOSTICS").as_deref() == Ok("1") { "ON" } else { "OFF" };
    let quiet = if env::var("KNIFE_VITA_QUIET").as_deref() == Ok("1") { "ON" } else { "OFF" };
    run(Command::new("cmake").arg("-S").arg(source).arg("-B").arg(&output)
        .arg("-G").arg("Ninja").arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!("-DKNIFE_VITA_DEV_DIAGNOSTICS={diagnostics}"))
        .arg(format!("-DKNIFE_VITA_QUIET={quiet}"))
        .arg(format!("-DCMAKE_TOOLCHAIN_FILE={sdk}/share/vita.toolchain.cmake")));
    run(Command::new("cmake").arg("--build").arg(&output));
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-search=native={}", PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../vita/vendor/dependencies/lib").display());
    println!("cargo:rustc-link-search=native={sdk}/arm-vita-eabi/lib");
    println!("cargo:rustc-link-lib=static=knife_vita_platform");
    println!("cargo:rustc-link-search=native={}", PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../vita/vendor/shacccg-ext").display());
    println!("cargo:rustc-link-lib=static=SceShaccCgExt");
    println!("cargo:rustc-link-lib=static=taihen_stub");
    for library in ["png", "jpeg", "z", "opusfile", "opus", "ogg", "m"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    for library in ["SceLibKernel_stub", "SceKernelThreadMgr_stub", "SceIofilemgr_stub", "SceProcessmgr_stub",
        "SceGxm_stub", "SceDisplay_stub", "SceSysmem_stub", "SceAudio_stub",
        "SceAvPlayer_stub", "SceSysmodule_stub", "SceJpeg_stub", "ScePower_stub",
        "SceShaccCg_stub_weak", "SceKernelModulemgr_stub"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
}
