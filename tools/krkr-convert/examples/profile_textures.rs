//! Measure encoding, packing and validation without replacing game resources.
use krkr_convert::bc::{Encoder, Quality, Storage};
use krkr_protocol::{graphics::Size, texture::Format};
fn main() {
    for path in std::env::args().skip(1) {
        let input = image::open(&path)
            .unwrap()
            .resize_exact(512, 512, image::imageops::FilterType::Lanczos3)
            .into_rgba8();
        let size = Size {
            width: 512,
            height: 512,
        };
        let format = if input.pixels().any(|p| p[3] != 255) {
            Format::Bc3Rgba
        } else {
            Format::Bc1Rgb
        };
        for storage in [Storage::Bc, Storage::BcCrunch] {
            let encoder = Encoder::new(Quality::Balanced);
            let start = std::time::Instant::now();
            let data = match storage {
                Storage::Bc => encoder.encode(size, &input, format),
                Storage::BcCrunch => encoder.encode_packed(size, &input, format),
            }
            .unwrap();
            let rejection = encoder.check(size, &input, &data, true).unwrap();
            println!(
                "{}",
                serde_json::json!({"source":path,"storage":storage,"wall_ms":start.elapsed().as_secs_f64()*1000.,"bytes":data.len(),"rejection":rejection,"stages":encoder.timings()})
            );
        }
    }
}
