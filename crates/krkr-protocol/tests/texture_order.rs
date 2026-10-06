use krkr_protocol::{
    graphics::Size,
    texture::{Format, reorder_bc, vita_block_index},
};

fn reference(size: Size, format: Format, source: &[u8], target: &mut [u8]) {
    let bytes = if format == Format::Bc3Rgba { 16 } else { 8 };
    let (width, height) = (size.width / 4, size.height / 4);
    for y in 0..height {
        for x in 0..width {
            let src = (y * width + x) as usize * bytes;
            let dst = vita_block_index(x, y, width, height) * bytes;
            target[dst..dst + bytes].copy_from_slice(&source[src..src + bytes]);
        }
    }
}

#[test]
fn bc_address_masks_match_reference_for_square_and_rectangular_tiles() {
    let mut sizes: Vec<_> = (3..=8)
        .flat_map(|w| {
            (3..=8).map(move |h| Size {
                width: 1 << w,
                height: 1 << h,
            })
        })
        .collect();
    sizes.extend([
        Size {
            width: 32768,
            height: 8,
        },
        Size {
            width: 8,
            height: 32768,
        },
        Size {
            width: 1024,
            height: 1024,
        },
    ]);
    for size in sizes {
        for format in [Format::Bc1Rgb, Format::Bc3Rgba] {
            let bytes = format.byte_len(size).unwrap();
            let input: Vec<_> = (0..bytes)
                .map(|i| ((i ^ (i >> 7) ^ (i >> 15)) * 37) as u8)
                .collect();
            let mut expected = vec![0; bytes];
            let mut actual = vec![0; bytes];
            reference(size, format, &input, &mut expected);
            reorder_bc(size, format, &input, &mut actual, true).unwrap();
            assert_eq!(actual, expected, "{size:?} {format:?}");
            reorder_bc(size, format, &actual, &mut expected, false).unwrap();
            assert_eq!(expected, input);
        }
    }
}

#[test]
#[ignore = "manual release-mode throughput comparison"]
fn compare_bc_reorder_throughput() {
    use std::{hint::black_box, time::Instant};
    let size = Size {
        width: 1024,
        height: 1024,
    };
    let format = Format::Bc3Rgba;
    let input = vec![0x73; format.byte_len(size).unwrap()];
    let mut output = vec![0; input.len()];
    let start = Instant::now();
    for _ in 0..200 {
        reference(
            black_box(size),
            format,
            black_box(&input),
            black_box(&mut output),
        );
        black_box(&output);
    }
    let before = start.elapsed();
    let start = Instant::now();
    for _ in 0..200 {
        reorder_bc(
            black_box(size),
            format,
            black_box(&input),
            black_box(&mut output),
            true,
        )
        .unwrap();
        black_box(&output);
    }
    eprintln!(
        "200 x 1024x1024 BC3: reference={before:?}, masks={:?}",
        start.elapsed()
    );
}
