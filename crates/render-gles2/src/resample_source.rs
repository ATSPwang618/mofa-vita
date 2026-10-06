//! Shared by the runtime and the SGX offline shader catalog.
pub const TAPS: [usize; 3] = [8, 32, 64];

pub fn fragment(taps: usize) -> String {
    format!(
        "#define FILTER_TAPS {taps}\n{}\n{}",
        include_str!("float.glsl"),
        include_str!("resample_direct.frag")
    )
}
