//! Host validation of the exact profile used by the native game entry.
fn main() {
    let root = std::env::args_os().nth(1).expect("usage: check_assets ASSET_DIRECTORY");
    let started = std::time::Instant::now();
    match knife_platform_vita::asset_check::check_assets(std::path::Path::new(&root)) {
        Ok(report) => println!("PASS files={} read_bytes={} elapsed_ms={:.3}",
            report.files, report.read_bytes, started.elapsed().as_secs_f64() * 1000.),
        Err(error) => { eprintln!("{error}"); std::process::exit(1); }
    }
}
