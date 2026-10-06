use krkr_protocol::{
    budget::Budget,
    graphics::{Adjustment, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_wgpu::{Gpu, Image};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn read(gpu: &Gpu, image: &Image, rect: Rect) -> Vec<u8> {
    let mut request = gpu.readback(image, rect, false).unwrap();
    let start = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = request.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn corrections_use_one_resident_plane_and_keep_exact_pixels_across_strips() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let size = Size {
        width: 600,
        height: 5,
    };
    let input: Vec<u8> = (0..size.width * size.height)
        .flat_map(|i| {
            [
                ((i * 31) % 256) as u8,
                ((i * 13) % 256) as u8,
                ((i * 7) % 256) as u8,
                ((i * 19) % 256) as u8,
            ]
        })
        .collect();
    let table = Arc::new(std::array::from_fn(|i| {
        [255 - i as u32, (i as u32 * 3 / 4), (i as u32 / 2), 0]
    }));
    let clip = Rect {
        left: 13,
        top: 1,
        width: 580,
        height: 3,
    };
    for operation in [
        Adjustment::Gamma {
            table: table.clone(),
            additive: false,
        },
        Adjustment::Gamma {
            table: table.clone(),
            additive: true,
        },
        Adjustment::GrayScale,
    ] {
        gpu.resident = Budget::new(size.rgba_bytes().unwrap());
        let mut data = Bytes::zeroed(input.len(), &gpu.staging).unwrap();
        data.as_mut_slice().copy_from_slice(&input);
        let mut image = gpu.reserve_upload(size, true, false).unwrap();
        gpu.upload(
            &mut image,
            &Pixels {
                size,
                main: Some(data),
                province: None,
            },
        )
        .unwrap();
        gpu.adjust(&mut image, clip, &operation).unwrap();
        let actual = read(&gpu, &image, size.rect());
        for y in 0..size.height {
            for x in 0..size.width {
                let at = ((y * size.width + x) * 4) as usize;
                let mut expected = input[at..at + 4].to_vec();
                if x >= clip.left as u32
                    && x < clip.left as u32 + clip.width
                    && y >= clip.top as u32
                    && y < clip.top as u32 + clip.height
                {
                    let a = u32::from(expected[3]);
                    match operation {
                        Adjustment::GrayScale => {
                            let gray = (u32::from(expected[0]) * 54
                                + u32::from(expected[1]) * 183
                                + u32::from(expected[2]) * 19)
                                >> 8;
                            expected[..3].fill(gray as u8);
                        }
                        Adjustment::Gamma { additive, .. } => {
                            for c in 0..3 {
                                let color = u32::from(expected[c]);
                                let value = if !additive {
                                    if a == 0 {
                                        color
                                    } else {
                                        table[color as usize][c]
                                    }
                                } else if a == 255 {
                                    table[color as usize][c]
                                } else if color > a {
                                    ((table[255][c] * (a + (a >> 7))) >> 8) + color - a
                                } else {
                                    let straight =
                                        (((65536 / a.max(1)).min(65535) * color) >> 8).min(255);
                                    (table[straight as usize][c] * (a + (a >> 7))) >> 8
                                };
                                expected[c] = value.min(255) as u8;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                assert_eq!(&actual[at..at + 4], expected, "pixel ({x},{y})");
            }
        }
        assert_eq!(gpu.resident.used(), size.rgba_bytes().unwrap());
        drop(image);
        gpu.trim_resident_pool();
    }
    gpu.resident = Budget::new(size.rgba_bytes().unwrap() * 2);
    let mut data = Bytes::zeroed(input.len(), &gpu.staging).unwrap();
    data.as_mut_slice().copy_from_slice(&input);
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(data),
            province: None,
        },
    )
    .unwrap();
    let frozen = image.shared_main();
    gpu.adjust(&mut image, clip, &Adjustment::GrayScale)
        .unwrap();
    let corrected = read(&gpu, &image, size.rect());
    assert_eq!(read(&gpu, &frozen, size.rect()), input);
    assert_ne!(corrected, input);
    drop((image, frozen));
    gpu.trim_resident_pool();
    // A deferred effect canvas is materialized once. Color correction must not
    // then demand another 18 MB resident texture at the same instant.
    let size = Size {
        width: 3000,
        height: 1500,
    };
    gpu.resident = Budget::new(size.rgba_bytes().unwrap());
    let mut image = gpu.create_image(size, 0x80602010).unwrap();
    gpu.adjust(&mut image, size.rect(), &Adjustment::GrayScale)
        .unwrap();
    assert_eq!(
        read(
            &gpu,
            &image,
            Rect {
                left: 2999,
                top: 1499,
                width: 1,
                height: 1
            }
        ),
        [44, 44, 44, 128]
    );
    assert_eq!(gpu.resident.used(), size.rgba_bytes().unwrap());
}
