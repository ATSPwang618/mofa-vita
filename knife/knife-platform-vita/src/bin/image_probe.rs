#[cfg(target_os = "vita")]
fn run() -> Result<(), String> {
    use std::fs::File;
    use std::io::Write;

    unsafe extern "C" {
        fn kv_init_data_dir() -> i32;
    }

    // SAFETY: the SDK wrapper takes no pointers and creates the fixed DEV data directory.
    let status = unsafe { kv_init_data_dir() };
    if status < 0 {
        return Err(format!(
            "create image probe data directory: SDK error {status:#010x}"
        ));
    }
    let path = knife_platform_vita::data_path("image-probe.log")?;
    let mut log = File::create(path).map_err(|error| error.to_string())?;
    writeln!(log, "Knife Vita DEV image probe v1; title ID KNIF00004")
        .map_err(|error| error.to_string())?;
    log.flush().map_err(|error| error.to_string())?;
    let result = knife_platform_vita::run_image_probe(&mut log);
    if let Err(ref error) = result {
        writeln!(log, "{error}").map_err(|error| error.to_string())?;
    }
    writeln!(
        log,
        "RESULT {}",
        if result.is_ok() { "PASS" } else { "FAIL" }
    )
    .map_err(|error| error.to_string())?;
    log.sync_all().map_err(|error| error.to_string())?;
    result
}

#[cfg(target_os = "vita")]
fn main() {
    if let Err(error) = run() {
        eprintln!("Vita image probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("This image probe requires the Vita target and native PNG/JPEG decoder execution");
    std::process::exit(2);
}
