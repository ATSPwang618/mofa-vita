#[cfg(target_os = "vita")]
fn main() {
    if let Err(error) = knife_platform_vita::run_gxm_probe() {
        eprintln!("Vita GXM probe failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("This probe requires the Vita target and actual GXM execution");
    std::process::exit(2);
}
