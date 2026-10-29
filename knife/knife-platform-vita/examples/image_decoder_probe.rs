//! Link-only development target: force the Rust wrapper and both C codecs into
//! a real Vita ELF without initializing GXM or touching game resources.
#[cfg(target_os = "vita")]
fn main() {
    knife_platform_vita::run_image_probe(&mut std::io::stdout())
        .expect("native PNG/JPEG decoder probe failed");
}

#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("This example targets Vita; use the standalone host decoder tests on a host.");
}
