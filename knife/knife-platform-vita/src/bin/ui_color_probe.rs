#[cfg(target_os = "vita")]
#[path = "../ui_color_probe.rs"]
mod ui_color_probe;

#[cfg(target_os = "vita")]
fn main() {
    if let Err(error) = ui_color_probe::run_ui_color_probe() {
        eprintln!("Vita synthetic UI color probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "vita"))]
fn main() {
    eprintln!("This probe requires the Vita target and direct GXM execution");
    std::process::exit(2);
}
