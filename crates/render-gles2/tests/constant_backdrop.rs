#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
#[allow(dead_code)]
mod traffic;
use krkr_protocol::{
    graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn uploaded(gpu: &Gpu, size: Size, pixels: &[u8]) -> Image {
    let mut bytes = Bytes::zeroed(pixels.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(pixels);
    gpu.assign_bitmap(
        None,
        &Pixels {
            size,
            main: Some(bytes),
            province: None,
        },
    )
    .unwrap()
}
fn bytes(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn constant_regions_match_sampled_backdrops_for_legacy_modes_and_preserve_aliases() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 8,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 13,
            height: 11,
        };
        let source = uploaded(
            &gpu,
            size,
            &(0..size.width * size.height)
                .flat_map(|i| {
                    [
                        (i * 31) as u8,
                        (i * 73) as u8,
                        (i * 17) as u8,
                        (i * 11) as u8,
                    ]
                })
                .collect::<Vec<_>>(),
        );
        for mode in 1..=28 {
            let Some(mode) = Blend::from_legacy(mode) else {
                continue;
            };
            for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
                let options = BlendOptions {
                    mode,
                    face,
                    opacity: 173,
                    hold_alpha: true,
                };
                if !options.accepts_face() {
                    continue;
                }
                let mut reference = uploaded(
                    &gpu,
                    size,
                    &[0x34, 0x65, 0x87, 0x91].repeat((size.width * size.height) as usize),
                );
                let mut direct = gpu.create_image(size, 0x91346587).unwrap();
                let snapshot = direct.shared();
                let first = Rect {
                    left: 1,
                    top: 1,
                    width: 6,
                    height: 5,
                };
                // The next draw overlaps previous writes and crosses tiles: it
                // must fall back where the remembered background is no longer valid.
                for area in [
                    first,
                    Rect {
                        left: 4,
                        top: 3,
                        width: 8,
                        height: 7,
                    },
                ] {
                    for image in [&mut direct, &mut reference] {
                        gpu.operate(
                            image,
                            &source,
                            area,
                            area.left,
                            area.top,
                            size.rect(),
                            options,
                        )
                        .unwrap();
                    }
                    assert_eq!(
                        bytes(&gpu, &direct),
                        bytes(&gpu, &reference),
                        "{work_framebuffer} {options:?} {area:?}"
                    );
                }
                assert_eq!(gpu.pixel(&snapshot, 2, 2, false).unwrap(), 0x91346587);
            }
        }
    }
}

#[test]
fn untouched_regions_skip_resolves_and_clearing_damage_restores_the_fast_path() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 64,
            height: 32,
        };
        let source = uploaded(&gpu, size, &[180, 81, 41, 123].repeat(64 * 32));
        let mut image = gpu.create_image(size, 0x78325476).unwrap();
        let options = BlendOptions {
            mode: Blend::Alpha,
            face: DrawFace::Alpha,
            opacity: 201,
            hold_alpha: false,
        };
        let left = Rect {
            left: 2,
            top: 2,
            width: 12,
            height: 20,
        };
        let right = Rect { left: 40, ..left };
        traffic::reset();
        for area in [left, right] {
            gpu.operate(
                &mut image,
                &source,
                area,
                area.left,
                area.top,
                size.rect(),
                options,
            )
            .unwrap();
        }
        assert_eq!(
            traffic::store_calls(),
            0,
            "uniform regions resolved an unnecessary backdrop"
        );
        assert_eq!(traffic::read_calls(), 0);
        let damage = Rect {
            left: 2,
            top: 2,
            width: 50,
            height: 20,
        };
        gpu.fill(
            &mut image,
            &[Fill {
                rectangle: damage,
                color: 0x78325476,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        traffic::reset();
        gpu.operate(
            &mut image,
            &source,
            left,
            left.left,
            left.top,
            size.rect(),
            options,
        )
        .unwrap();
        assert_eq!(
            traffic::store_calls(),
            0,
            "restored background was not recognized"
        );
        assert_eq!(gpu.pixel(&image, 30, 10, false).unwrap(), 0x78325476);
        assert_ne!(gpu.pixel(&image, 3, 3, false).unwrap(), 0x78325476);
    }
}
