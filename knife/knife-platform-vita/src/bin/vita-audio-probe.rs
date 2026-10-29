#[cfg(target_os = "vita")]
fn main() {
    use std::{fs::File, io::Write};
    unsafe extern "C" { fn kv_init_data_dir() -> i32; }
    // SAFETY: SDK platform routine has no pointer parameters.
    let init = unsafe { kv_init_data_dir() };
    if init < 0 { eprintln!("audio probe directory: {init:#010x}"); std::process::exit(1); }
    let mut log = File::create("ux0:data/knife-woh-dev/audio-probe.log")
        .expect("audio probe log open");
    let result = knife_platform_vita::run_audio_probe(&mut log);
    if let Err(ref error) = result { writeln!(log, "{error}").expect("audio error log"); }
    writeln!(log, "RESULT {}", if result.is_ok() { "PASS" } else { "FAIL" })
        .expect("audio final log");
    log.sync_all().expect("audio probe log sync");
    if let Err(error) = result { eprintln!("AudioOut probe: {error}"); std::process::exit(1); }
}
#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("Vita AudioOut probe requires SceAudioOut on the Vita target");
    std::process::exit(2);
}
