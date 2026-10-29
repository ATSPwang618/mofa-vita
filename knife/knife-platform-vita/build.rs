use std::{env, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    let status = command.status().expect("start native build tool");
    assert!(status.success(), "native build failed: {command:?}");
}

fn main() {
    println!("cargo:rerun-if-env-changed=VITASDK");
    println!("cargo:rerun-if-env-changed=KNIFE_VITA_DEV_DIAGNOSTICS");
    println!("cargo:rerun-if-changed=../vita/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../vita/platform.c");
    println!("cargo:rerun-if-changed=../vita/platform.h");
    println!("cargo:rerun-if-changed=../vita/gxm_backend.c");
    println!("cargo:rerun-if-changed=../vita/gxm_backend.h");
    println!("cargo:rerun-if-changed=../vita/effects_backend.inc");
    println!("cargo:rerun-if-changed=../vita/shaders/generated/effects.h");
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
    run(Command::new("cmake").arg("-S").arg(source).arg("-B").arg(&output)
        .arg("-G").arg("Ninja").arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!("-DKNIFE_VITA_DEV_DIAGNOSTICS={diagnostics}"))
        .arg(format!("-DCMAKE_TOOLCHAIN_FILE={sdk}/share/vita.toolchain.cmake")));
    run(Command::new("cmake").arg("--build").arg(&output));
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-search=native={sdk}/arm-vita-eabi/lib");
    println!("cargo:rustc-link-lib=static=knife_vita_platform");
    for library in ["png", "jpeg", "z", "opusfile", "opus", "ogg", "m"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    for library in ["SceKernelThreadMgr_stub", "SceIofilemgr_stub", "SceProcessmgr_stub",
        "SceGxm_stub", "SceDisplay_stub", "SceSysmem_stub", "SceAudio_stub",
        "SceAvPlayer_stub", "SceSysmodule_stub", "SceJpeg_stub", "ScePower_stub"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
}
