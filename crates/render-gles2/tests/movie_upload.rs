#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn full_movie_upload_preserves_shared_frames_and_province() {
    let context = support::Context::new();
    for edge in [16, 64] {
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: edge,
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 48,
            height: 32,
        };
        let mut target = gpu.reserve_upload(size, true, true).unwrap();
        gpu.fill(
            &mut target,
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
        let snapshot = target.shared();
        let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, b) in data.as_mut_slice().iter_mut().enumerate() {
            *b = (i * 71) as u8;
        }
        let pixels = Pixels {
            size,
            main: Some(data),
            province: None,
        };
        traffic::reset();
        gpu.copy_pixels(&mut target, &pixels, false, size).unwrap();
        let cost = (
            traffic::draw_calls(),
            traffic::texture_allocations(),
            traffic::loaded_pixels(),
            traffic::stored_pixels(),
        );
        println!("movie edge={edge} draws/alloc/load/store={cost:?}");
        assert_eq!(
            (cost.0, cost.2, cost.3),
            (0, 0, 0),
            "full upload needs no GPU drawing or copies"
        );
        assert_eq!(
            gpu.readback(&target, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            pixels.main.as_ref().unwrap().as_slice()
        );
        assert_eq!(
            gpu.readback(&snapshot, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            [96, 64, 32, 128].repeat(48 * 32)
        );
        assert_eq!(
            gpu.readback(&target, size.rect(), true)
                .unwrap()
                .data
                .as_slice(),
            [17; 48 * 32]
        );
    }
}
