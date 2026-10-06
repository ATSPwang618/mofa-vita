#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Rect, Size},
    hit::Data,
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn local_patch_preserves_pending_writes_snapshots_and_province() {
    let context = support::Context::new();
    for work in [false, true] {
        for edge in [16, 64] {
            for shared in [false, true] {
                let gpu = unsafe {
                    Gpu::new(
                        context.gl_with(traffic::intercept),
                        Config {
                            tile_edge: edge,
                            work_framebuffer: work,
                            ..Default::default()
                        },
                    )
                    .unwrap()
                };
                let size = Size {
                    width: 48,
                    height: 32,
                };
                let mut image = gpu.reserve_upload(size, true, true).unwrap();
                gpu.fill(
                    &mut image,
                    &[
                        Fill {
                            rectangle: size.rect(),
                            color: 0x80604020,
                            face: DrawFace::Alpha,
                            hold_alpha: false,
                        },
                        Fill {
                            rectangle: size.rect(),
                            color: 17,
                            face: DrawFace::Province,
                            hold_alpha: false,
                        },
                    ],
                )
                .unwrap();
                let snapshot = shared.then(|| image.shared());
                let patch = Rect {
                    left: 5,
                    top: 7,
                    width: 16,
                    height: 9,
                };
                let mut data = Bytes::zeroed(16 * 9 * 4, &gpu.staging).unwrap();
                for (i, b) in data.as_mut_slice().iter_mut().enumerate() {
                    *b = (i * 71) as u8;
                }
                let input = Pixels {
                    size: Size {
                        width: 16,
                        height: 9,
                    },
                    main: Some(data),
                    province: None,
                };
                traffic::reset();
                gpu.patch_region(&mut image, patch, &input).unwrap();
                println!(
                    "patch work={work} edge={edge} shared={shared} draws={} textures={} stores={} loads={}",
                    traffic::draw_calls(),
                    traffic::texture_allocations(),
                    traffic::stored_pixels(),
                    traffic::loaded_pixels()
                );
                if edge == 64 && !shared {
                    assert_eq!(traffic::texture_allocations(), 0);
                    assert_eq!(traffic::draw_calls(), 0);
                }
                let mut expected = [96, 64, 32, 128].repeat(48 * 32);
                for row in 0..9 {
                    expected[((row + 7) * 48 + 5) * 4..][..16 * 4].copy_from_slice(
                        &input.main.as_ref().unwrap().as_slice()[row * 16 * 4..][..16 * 4],
                    );
                }
                assert_eq!(
                    gpu.readback(&image, size.rect(), false)
                        .unwrap()
                        .data
                        .as_slice(),
                    expected
                );
                assert_eq!(
                    gpu.readback(&image, size.rect(), true)
                        .unwrap()
                        .data
                        .as_slice(),
                    [17; 48 * 32]
                );
                if let Some(snapshot) = snapshot {
                    assert_eq!(
                        gpu.readback(&snapshot, size.rect(), false)
                            .unwrap()
                            .data
                            .as_slice(),
                        [96, 64, 32, 128].repeat(48 * 32)
                    );
                }
            }
        }
    }
}

#[test]
fn uniform_hit_planes_avoid_readback_and_real_alpha_changes_still_invalidate() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                tile_edge: 16,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 32,
        height: 16,
    };
    let mut image = gpu.reserve_upload(size, true, true).unwrap();
    for x in [0, 16] {
        gpu.fill(
            &mut image,
            &[Fill {
                rectangle: Rect {
                    left: x,
                    top: 0,
                    width: 16,
                    height: 16,
                },
                color: if x == 0 { 0x80123456 } else { 0x80654321 },
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
    }
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: size.rect(),
            color: 17,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    traffic::reset();
    let main = gpu.read_hit_plane(&image, false).unwrap();
    println!("uniform main hit read_calls={}", traffic::read_calls());
    assert_eq!(traffic::read_calls(), 0);
    let province = gpu.read_hit_plane(&image, true).unwrap();
    // Masked province fills currently have no tracked full-color background.
    // They must keep the readback fallback even when the actual plane is flat.
    assert_eq!(traffic::read_calls(), 2);
    assert!(matches!(main.data, Data::Uniform(128)));
    assert!(matches!(province.data, Data::Uniform(17)));
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 7,
                top: 3,
                width: 1,
                height: 1,
            },
            color: 31,
            face: DrawFace::Mask,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let hit = gpu.read_hit_plane(&image, false).unwrap();
    assert_eq!(hit.sample(7, 3), 31);
    assert_eq!(hit.sample(8, 3), 128);
    assert_eq!(hit.sample(-1, 3), 0);
}
