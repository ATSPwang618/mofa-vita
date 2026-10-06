use krkr_protocol::{
    budget::Budget,
    graphics::{Adjustment, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_wgpu::{Gpu, Image};
use std::time::{Duration, Instant};

fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let started = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn bounded_blur_preserves_seams_alpha_clipping_and_shared_source() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let size = Size {
        width: 700,
        height: 9,
    };
    // The old full intermediate requires 100,800 bytes. A 256-column strip
    // needs 36,864 bytes, even when the vertical halo covers the full image.
    gpu.scratch = Budget::new(40_000);
    let clip = Rect {
        left: 7,
        top: 2,
        width: 687,
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
    for (radius, alpha) in [([7, 3], false), ([7, 3], true), ([23, 8], true)] {
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
        // Test both unique-source COW and a caller's already-shared image.
        let shared = alpha.then(|| image.shared_main());
        gpu.adjust(&mut image, clip, &Adjustment::BoxBlur { radius, alpha })
            .unwrap();
        let actual = read(&gpu, &image);
        if let Some(shared) = shared {
            assert_eq!(read(&gpu, &shared), input);
        }
        for y in 0..size.height {
            for x in 0..size.width {
                let at = ((y * size.width + x) * 4) as usize;
                let mut expected = input[at..at + 4]
                    .iter()
                    .map(|v| u32::from(*v))
                    .collect::<Vec<_>>();
                if x >= clip.left as u32
                    && x < clip.left as u32 + clip.width
                    && y >= clip.top as u32
                    && y < clip.top as u32 + clip.height
                {
                    let mut sum = [0u32; 4];
                    let mut count = 0;
                    for sy in y.saturating_sub(radius[1])..=(y + radius[1]).min(size.height - 1) {
                        for sx in x.saturating_sub(radius[0])..=(x + radius[0]).min(size.width - 1)
                        {
                            let src = ((sy * size.width + sx) * 4) as usize;
                            let a = u32::from(input[src + 3]);
                            for c in 0..4 {
                                let value = u32::from(input[src + c]);
                                sum[c] += if alpha && c < 3 {
                                    (value * (a + (a >> 7))) >> 8
                                } else {
                                    value
                                };
                            }
                            count += 1;
                        }
                    }
                    for c in 0..4 {
                        expected[c] = if (radius[0] * 2 + 1) * (radius[1] * 2 + 1) < 256 {
                            ((sum[c] + count / 2) * (65536 / count)) >> 16
                        } else {
                            (sum[c] + count / 2) / count
                        };
                    }
                    if alpha {
                        for c in 0..3 {
                            expected[c] = (expected[c] * 255)
                                .checked_div(expected[3])
                                .unwrap_or(0)
                                .min(255);
                        }
                    }
                }
                assert_eq!(
                    actual[at..at + 4],
                    expected.iter().map(|v| *v as u8).collect::<Vec<_>>(),
                    "pixel ({x},{y}), radius={radius:?}, alpha={alpha}"
                );
            }
        }
        assert!(gpu.scratch.used() <= 40_000);
    }
}
