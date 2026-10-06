#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
#[path = "support/feedback.rs"]
mod feedback;
mod support;
use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, ImageRef, Node, Scene, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};
use std::{collections::HashMap, sync::Arc};

#[test]
fn compact_sky_has_same_pixels_across_tile_and_gather_boundaries() {
    let context = support::Context::new();
    let audit = feedback::Check::new(context.gl());
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    let physical = Size {
        width: 960,
        height: 540,
    };
    let stored = Size {
        width: 1024,
        height: 1024,
    };
    let mut reference: Vec<Vec<u8>> = Vec::new();
    for (edge, targets) in [(2048, 0), (1024, 8), (512, 0), (512, 8)] {
        let budget = Budget::new(120 * 1024 * 1024);
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(feedback::intercept),
                Config {
                    resident: budget.child(budget.limit()),
                    scratch: budget.child(48 * 1024 * 1024),
                    staging: Budget::new(32 * 1024 * 1024),
                    tile_edge: edge,
                    work_framebuffer: true,
                    render_target_cache_entries: targets,
                    render_target_cache_bytes: 8 * 1024 * 1024,
                    canvas_limit: Some(physical),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(logical);
        let mut bytes = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (index, pixel) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            let x = index as u32 % stored.width;
            let y = index as u32 / stored.width;
            pixel.copy_from_slice(&[
                (30 + x * 70 / stored.width) as u8,
                (60 + y * 130 / stored.height) as u8,
                (160 + (x + y) * 70 / (stored.width + stored.height)) as u8,
                255,
            ]);
        }
        let image = gpu
            .upload_scaled(
                &Pixels {
                    size: stored,
                    main: Some(bytes),
                    province: None,
                },
                logical,
            )
            .unwrap();
        let mut ids = slotmap::SlotMap::with_key();
        let id = ids.insert(());
        let images = HashMap::from([(id, image)]);
        for (case, blend) in [Blend::Opaque, Blend::Alpha].into_iter().enumerate() {
            let scene = Scene {
                nodes: vec![Node {
                    parent: None,
                    cache: Some(Arc::new(())),
                    visible: true,
                    opacity: 255,
                    image: Some(ImageRef {
                        id,
                        lifetime: Arc::default(),
                    }),
                    rectangle: logical.rect(),
                    image_left: 0,
                    image_top: 0,
                    blend,
                    neutral_color: 0,
                }],
                ..Default::default()
            };
            let result = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            let pixels = gpu
                .readback(&result, result.size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec();
            if edge == 2048 {
                reference.push(pixels);
            } else {
                let expected = &reference[case];
                assert_eq!(pixels.len(), expected.len());
                let bad: Vec<_> = pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(expected.as_chunks::<4>().0.iter())
                    .enumerate()
                    .filter(|(_, (a, b))| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 1))
                    .take(8)
                    .map(|(i, (a, b))| {
                        (
                            i % physical.width as usize,
                            i / physical.width as usize,
                            a.to_vec(),
                            b.to_vec(),
                        )
                    })
                    .collect();
                assert!(
                    bad.is_empty(),
                    "edge={edge} cache={targets} blend={blend:?}: {bad:?}"
                );
            }
        }
    }
    assert!(audit.conflicts().is_empty(), "{:?}", audit.conflicts());
}
