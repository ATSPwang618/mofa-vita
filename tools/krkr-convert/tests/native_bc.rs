use krkr_convert::bc::Encoder;
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::Bytes,
    texture::{Compressed, Format},
};
use std::{fs, path::PathBuf, process::Command, sync::atomic::AtomicBool};

fn word(data: &[u8], at: usize) -> usize {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize
}

#[test]
fn smooth_color_ramp_does_not_collapse_to_wide_quantization_bands() {
    let encoder = Encoder::new(Default::default());
    let size = Size {
        width: 256,
        height: 128,
    };
    let pixels: Vec<u8> = (0..size.width * size.height)
        .flat_map(|i| {
            let value = ((i % size.width) / 8) as u8;
            [100 + value, 110 + value, 125 + value, 255]
        })
        .collect();
    for format in [Format::Bc1Rgb, Format::Bc3Rgba] {
        let data = encoder.encode(size, &pixels, format).unwrap();
        let offset = 68 + word(&data, 60);
        let budget = Budget::new(1 << 20);
        let mut bytes = Bytes::zeroed(data.len(), &budget).unwrap();
        bytes.as_mut_slice().copy_from_slice(&data);
        let compressed = Compressed::new(size, format, bytes, offset).unwrap();
        let decoded = krkr_image::compressed::decode(&compressed, &budget, &AtomicBool::new(false))
            .unwrap()
            .main
            .unwrap();
        // Large correlated errors cause contour bands even when pixel RMSE is
        // small. Average over 8x8 pixels to distinguish those from fine dither.
        let mut squared = 0.0f64;
        let mut count = 0;
        for y in (0..size.height).step_by(8) {
            for x in (0..size.width).step_by(8) {
                let mut error = [0i32; 3];
                for dy in 0..8 {
                    for dx in 0..8 {
                        let at = (((y + dy) * size.width + x + dx) * 4) as usize;
                        for c in 0..3 {
                            error[c] +=
                                i32::from(decoded.as_slice()[at + c]) - i32::from(pixels[at + c]);
                        }
                        assert_eq!(decoded.as_slice()[at + 3], 255);
                    }
                }
                squared += error
                    .iter()
                    .map(|&v| (f64::from(v) / 64.0).powi(2))
                    .sum::<f64>();
                count += 3;
            }
        }
        let coarse_rmse = (squared / f64::from(count)).sqrt();
        assert!(
            coarse_rmse < 1.5,
            "{format:?}: coherent color error {coarse_rmse}"
        );
    }
}

#[test]
#[ignore = "requires KRKR_GXT_TOOL (official layout tool)"]
fn bc_native_layout_matches_official_gxt_tool_and_preserves_alpha() {
    let gxt_tool = PathBuf::from(std::env::var_os("KRKR_GXT_TOOL").unwrap());
    let encoder = Encoder::new(Default::default());
    let temp = tempfile::tempdir().unwrap();
    for (width, height) in [(8, 8), (128, 64), (64, 128), (32, 32)] {
        let size = Size { width, height };
        for format in [Format::Bc1Rgb, Format::Bc3Rgba] {
            let pixels: Vec<u8> = (0..width * height)
                .flat_map(|i| {
                    let (x, y) = (i % width, i / width);
                    // Smooth color plus opaque, transparent and gradient-alpha areas.
                    [
                        ((x * 255) / (width - 1)) as u8,
                        ((y * 255) / (height - 1)) as u8,
                        80,
                        if format.opaque() || x < width / 3 {
                            255
                        } else if x >= 2 * width / 3 {
                            0
                        } else {
                            (y * 255 / (height - 1)) as u8
                        },
                    ]
                })
                .collect();
            let linear = encoder.encode(size, &pixels, format).unwrap();
            let tags = vec![
                ("names".into(), "青子\0表情".into()),
                ("offs_x".into(), "-10".into()),
            ];
            let tagged =
                krkr_image::compressed::assemble(size, std::slice::from_ref(&linear), &tags)
                    .unwrap();
            let native = krkr_image::compressed::vita_bc(&tagged).unwrap();
            let offset = 68 + word(&native, 60);
            assert_eq!(
                word(&native, 28) as u32,
                format.vita().unwrap().gl_internal()
            );
            let dds = temp.path().join("linear.dds");
            let gxt = temp.path().join("native.gxt");
            // Wrap the already encoded blocks, without a second lossy encode.
            let blocks = &linear[68 + word(&linear, 60)..];
            let mut header = [0u8; 128];
            header[..4].copy_from_slice(b"DDS ");
            for (at, value) in [
                (4, 124),
                (8, 0x81007),
                (12, height),
                (16, width),
                (20, blocks.len() as u32),
                (76, 32),
                (80, 4),
                (108, 0x1000),
            ] {
                header[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            header[84..88].copy_from_slice(if format.opaque() { b"DXT1" } else { b"DXT5" });
            fs::write(&dds, [header.as_slice(), blocks].concat()).unwrap();
            let output = Command::new(&gxt_tool)
                .arg("-i")
                .arg(&dds)
                .arg("-o")
                .arg(&gxt)
                .output()
                .unwrap();
            assert!(output.status.success(), "{:?}", output);
            let golden = fs::read(&gxt).unwrap();
            let golden_offset = word(&golden, 32);
            let length = format.byte_len(size).unwrap();
            assert_eq!(
                &native[offset..],
                &golden[golden_offset..golden_offset + length],
                "{format:?} {size:?}"
            );
            let budget = Budget::new(1 << 20);
            let mut bytes = Bytes::zeroed(native.len(), &budget).unwrap();
            bytes.as_mut_slice().copy_from_slice(&native);
            let compressed = Compressed::new(size, format.vita().unwrap(), bytes, offset).unwrap();
            let decoded =
                krkr_image::compressed::decode(&compressed, &budget, &AtomicBool::new(false))
                    .unwrap();
            let actual = decoded.main.unwrap();
            for (src, dst) in pixels
                .as_chunks::<4>()
                .0
                .iter()
                .zip(actual.as_slice().as_chunks::<4>().0.iter())
            {
                if src[3] == 0 || src[3] == 255 {
                    assert_eq!(dst[3], src[3]);
                }
            }
            println!(
                "{format:?} {width}x{height}: {length} native bytes match official GXT, alpha endpoints preserved"
            );
        }
    }
}
