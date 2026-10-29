#[cfg(target_os = "vita")]
fn main() {
    if let Err(error) = knife_platform_vita::run_platform_probe() {
        eprintln!("Vita platform probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("This probe requires a Vita target. Host validation: cargo test -p knife-platform-vita --lib");
    std::process::exit(2);
}
