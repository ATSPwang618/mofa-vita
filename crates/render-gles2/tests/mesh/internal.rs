use super::*;
use crate::Config;
use crate::test_support as support;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, ImageLifetime, ImageRef},
    mesh::{Geometry, Vertex},
    pixels::Bytes,
};

fn quad(gpu: &Gpu, texture: Texture, left: f32, right: f32) -> Draw {
    Draw {
        geometry: Geometry {
            vertices: [[left, -1.], [right, -1.], [right, 1.], [left, 1.]]
                .into_iter()
                .map(|position| Vertex {
                    position,
                    uv: [0.5; 2],
                })
                .collect(),
            indices: vec![0, 1, 2, 2, 3, 0],
            _permit: gpu.staging.reserve(4 * 16 + 6 * 2).unwrap(),
        },
        texture,
        blend: Blend::AlphaMax,
        opacity: 1.,
        color: [1.; 4],
        solid_color: false,
        masks: vec![],
        visible: true,
    }
}
fn asset(gpu: &Gpu, size: Size, color: &[u8]) -> Arc<Pixels> {
    let mut bytes = Bytes::zeroed(color.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(color);
    Arc::new(Pixels {
        size,
        main: Some(bytes),
        province: None,
    })
}
fn color(gpu: &Gpu, rgba: [u8; 4]) -> Arc<Pixels> {
    asset(
        gpu,
        Size {
            width: 1,
            height: 1,
        },
        &rgba,
    )
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn near(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (&a, &b)) in actual.iter().zip(expected).enumerate() {
        assert!(a.abs_diff(b) <= 1, "byte {i}: {a} != {b}");
    }
}

#[test]
fn masks_use_one_bounded_surface_and_assets_stay_resident_until_released() {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 128,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 960,
        height: 544,
    };
    let canvas_bytes = size.rgba_bytes().unwrap();
    gpu.resident = Budget::new(canvas_bytes * 2 + 64);
    gpu.scratch = Budget::new(128 * 128 * 4);
    let baseline = gpu.staging.used();
    let red = color(&gpu, [255, 0, 0, 255]);
    let white = color(&gpu, [255; 4]);
    let mut left_mask = quad(&gpu, Texture::Pixels(white.clone()), -1., 0.);
    left_mask.opacity = 0.5;
    // Nested masks are deliberately ignored while building a mask.
    left_mask.masks = vec![1];
    let mut right_mask = quad(&gpu, Texture::Pixels(white.clone()), 0., 1.);
    right_mask.opacity = 0.49;
    let mut left = quad(&gpu, Texture::Pixels(red.clone()), -1., 1.);
    left.masks = vec![0];
    let mut right = quad(&gpu, Texture::Pixels(red.clone()), -1., 1.);
    right.masks = vec![1];
    let batch = Batch {
        draws: vec![left_mask, right_mask, left, right],
        order: vec![2, 3, 2],
        clear: Some([0.; 4]),
    };
    let images = HashMap::new();
    let mut target = gpu.create_image(size, 0).unwrap();
    assert_eq!(gpu.mesh_upload_bytes(&batch).unwrap(), 8);
    let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
    assert!(
        gpu.meshes.borrow().as_ref().unwrap().blend_max,
        "Mesa must expose the fast path tested here"
    );
    let original = prepared
        .textures
        .iter()
        .map(|i| i.main.clone())
        .collect::<Vec<_>>();
    gpu.draw_meshes(&mut target, prepared).unwrap();
    assert_eq!(gpu.mesh_upload_bytes(&batch).unwrap(), 0);
    let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
    for (old, new) in original.iter().zip(&prepared.textures) {
        assert!(Rc::ptr_eq(
            old.as_ref().unwrap(),
            new.main.as_ref().unwrap()
        ));
    }
    gpu.draw_meshes(&mut target, prepared).unwrap();
    for (i, pixel) in read(&gpu, &target).as_chunks::<4>().0.iter().enumerate() {
        let expected = if i % 960 < 480 {
            [255, 0, 0, 255]
        } else {
            [0; 4]
        };
        assert_eq!(*pixel, expected, "mask/triangle seam at {i}");
    }
    assert_eq!(gpu.scratch.used(), 128 * 128 * 4);
    assert_eq!(gpu.resident.used(), canvas_bytes + 8);
    drop(original);
    drop(batch);
    drop(red);
    drop(white);
    gpu.collect_mesh_textures();
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), canvas_bytes);
    assert_eq!(
        gpu.staging.used(),
        baseline,
        "CPU arrays and GPU buffers must retire their permits"
    );
}

#[test]
fn all_mesh_blends_and_alpha_max_without_extensions_follow_the_same_rules() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let source = color(&gpu, [204, 102, 51, 128]);
    let images = HashMap::new();
    let mut target = gpu
        .create_image(
            Size {
                width: 1,
                height: 1,
            },
            0,
        )
        .unwrap();
    for use_extension in [true, false] {
        for (blend, expected) in [
            (Blend::AlphaMax, [70, 64, 70, 102]),
            (Blend::MultiplyAdd, [46, 71, 92, 102]),
            (Blend::Alpha, [70, 64, 70, 92]),
            (Blend::LayerAlpha, [69, 64, 70, 92]),
        ] {
            let mut draw = quad(&gpu, Texture::Pixels(source.clone()), -1., 1.);
            draw.opacity = 0.5;
            draw.blend = blend;
            let batch = Batch {
                draws: vec![draw],
                order: vec![0],
                clear: Some([0.1, 0.2, 0.3, 0.4]),
            };
            let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
            gpu.meshes.borrow_mut().as_mut().unwrap().blend_max = use_extension;
            gpu.draw_meshes(&mut target, prepared).unwrap();
            near(&read(&gpu, &target), &expected);
        }
    }
    let mut draw = quad(&gpu, Texture::Pixels(source.clone()), -1., 1.);
    draw.solid_color = true;
    draw.color = [0., 1., 0., 1.];
    draw.blend = Blend::Alpha;
    let solid = Batch {
        draws: vec![draw],
        order: vec![0],
        clear: Some([0.; 4]),
    };
    gpu.draw_meshes(&mut target, gpu.prepare_meshes(&solid, &images).unwrap())
        .unwrap();
    near(&read(&gpu, &target), &[0, 128, 0, 64]);
    // Several overlapping triangles inside one draw need increasing alpha even
    // when a later triangle has less alpha. Per-draw snapshots are insufficient.
    let texture = asset(
        &gpu,
        Size {
            width: 3,
            height: 1,
        },
        &[255, 0, 0, 128, 0, 255, 0, 240, 0, 0, 255, 64],
    );
    let mut overlap = quad(&gpu, Texture::Pixels(texture), -1., 1.);
    overlap.geometry.vertices.clear();
    overlap.geometry.indices.clear();
    for (i, u) in [5. / 6., 0.5, 1. / 6.].into_iter().enumerate() {
        for position in [[-1., -1.], [3., -1.], [-1., 3.]] {
            overlap.geometry.vertices.push(Vertex {
                position,
                uv: [u, 0.5],
            });
            overlap
                .geometry
                .indices
                .push((i * 3 + overlap.geometry.indices.len() % 3) as u16);
        }
    }
    let batch = Batch {
        draws: vec![overlap],
        order: vec![0],
        clear: Some([0.; 4]),
    };
    let mut outputs = Vec::new();
    for use_extension in [true, false] {
        let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
        gpu.meshes.borrow_mut().as_mut().unwrap().blend_max = use_extension;
        gpu.draw_meshes(&mut target, prepared).unwrap();
        outputs.push(read(&gpu, &target));
    }
    near(&outputs[0], &outputs[1]);
    near(&outputs[1], &[128, 120, 2, 240]);
    assert_eq!(outputs[1][3], 240);
    // Following ordinary draw commands must not inherit alpha-only masks or
    // mesh vertex state from the two-pass fallback.
    let rectangle = target.size.rect();
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle,
            color: 0xff2468ac,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(read(&gpu, &target), [0x24, 0x68, 0xac, 255]);
}

#[test]
fn compact_mesh_uvs_interpolate_across_tile_corners_without_materialization() {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let physical = Size {
        width: 4,
        height: 4,
    };
    let logical = Size {
        width: 8,
        height: 8,
    };
    let data: Vec<_> = (0..16)
        .flat_map(|i| {
            [
                (i * 13 + 20) as u8,
                (i * 43 + 7) as u8,
                (i * 71 + 11) as u8,
                255,
            ]
        })
        .collect();
    let pixels = asset(&gpu, physical, &data);
    let source = gpu
        .logical_image(gpu.assign_bitmap(None, &pixels).unwrap(), logical)
        .unwrap();
    let mut ids = slotmap::SlotMap::<ImageId, ()>::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::new(ImageLifetime::default()),
    };
    let images = HashMap::from([(reference.id, source)]);
    let mut draw = quad(&gpu, Texture::Image(reference), -1., 1.);
    for (v, uv) in draw
        .geometry
        .vertices
        .iter_mut()
        .zip([[0., 0.], [1., 0.], [1., 1.], [0., 1.]])
    {
        v.uv = uv;
    }
    let batch = Batch {
        draws: vec![draw],
        order: vec![0],
        clear: Some([0.; 4]),
    };
    let size = Size {
        width: 7,
        height: 7,
    };
    let mut target = gpu.create_image(size, 0).unwrap();
    let scratch = gpu.scratch.clone();
    gpu.scratch = Budget::new(0);
    gpu.draw_meshes(&mut target, gpu.prepare_meshes(&batch, &images).unwrap())
        .unwrap();
    assert_eq!(gpu.scratch.used(), 0);
    gpu.scratch = scratch;
    let actual = read(&gpu, &target);
    for y in 0..7 {
        for x in 0..7 {
            let sx = ((x as f64 + 0.5) * 8. / 7. - 0.5).clamp(0., 7.);
            let sy = ((y as f64 + 0.5) * 8. / 7. - 0.5).clamp(0., 7.);
            let (ix, iy) = (sx.floor() as usize, sy.floor() as usize);
            let (fx, fy) = (sx.fract(), sy.fract());
            let mut expected = [0.; 4];
            for (dx, dy, w) in [
                (0, 0, (1. - fx) * (1. - fy)),
                (1, 0, fx * (1. - fy)),
                (0, 1, (1. - fx) * fy),
                (1, 1, fx * fy),
            ] {
                let from = (((iy + dy).min(7) / 2) * 4 + (ix + dx).min(7) / 2) * 4;
                for c in 0..4 {
                    expected[c] += f64::from(data[from + c]) * w;
                }
            }
            near(
                &actual[(y * 7 + x) * 4..(y * 7 + x + 1) * 4],
                &expected.map(|v| v.round() as u8),
            );
        }
    }
}

#[test]
fn alpha_max_fallback_masks_reuse_border_tiles_and_ignore_disabled_masks() {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 3,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.scratch = Budget::new(2 * 3 * 3 * 4);
    let size = Size {
        width: 7,
        height: 5,
    };
    let mut target = gpu.create_image(size, 0).unwrap();
    let mut mask = quad(&gpu, Texture::Pixels(color(&gpu, [255; 4])), -1., -1. / 7.);
    mask.opacity = 0.5;
    let mut draw = quad(
        &gpu,
        Texture::Pixels(color(&gpu, [255, 0, 0, 255])),
        -1.,
        1.,
    );
    draw.masks = vec![0];
    let mut batch = Batch {
        draws: vec![mask, draw],
        order: vec![1],
        clear: Some([0.; 4]),
    };
    let images = HashMap::new();
    for mode in 0..3 {
        batch.draws[0].opacity = if mode == 1 { 0. } else { 0.5 };
        batch.draws[0].visible = mode != 2;
        let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
        gpu.meshes.borrow_mut().as_mut().unwrap().blend_max = false;
        gpu.draw_meshes(&mut target, prepared).unwrap();
        for (i, pixel) in read(&gpu, &target).as_chunks::<4>().0.iter().enumerate() {
            let visible = mode == 1 || (mode == 0 && i % 7 < 3);
            assert_eq!(
                *pixel,
                if visible { [255, 0, 0, 255] } else { [0; 4] },
                "mode {mode}, pixel {i}"
            );
        }
    }
}

#[test]
fn mesh_preparation_pins_aliases_and_rejection_keeps_the_target_unchanged() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 4,
        height: 3,
    };
    let mut ids = slotmap::SlotMap::<ImageId, ()>::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::new(ImageLifetime::default()),
    };
    let mut image = gpu.create_image(size, 0x8000ff00).unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: size.rect(),
            color: 61,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let mut images = HashMap::from([(reference.id, image)]);
    let batch = Batch {
        draws: vec![quad(&gpu, Texture::Image(reference.clone()), -1., 1.)],
        order: vec![0],
        clear: Some([0.; 4]),
    };
    let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
    let saved = prepared.textures[0].shared();
    gpu.draw_meshes(images.get_mut(&reference.id).unwrap(), prepared)
        .unwrap();
    assert_eq!(read(&gpu, &saved), [0, 255, 0, 128].repeat(12));
    near(
        &read(&gpu, &images[&reference.id]),
        &[0, 128, 0, 128].repeat(12),
    );
    assert_eq!(
        gpu.readback(&images[&reference.id], size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &[61; 12]
    );
    let before = read(&gpu, &images[&reference.id]);
    let mut invalid = Batch {
        draws: vec![quad(&gpu, Texture::Image(reference.clone()), -1., 1.)],
        order: vec![0],
        clear: Some([1.; 4]),
    };
    invalid.draws[0].geometry.indices[0] = 4;
    assert!(gpu.prepare_meshes(&invalid, &images).is_err());
    invalid.draws[0].geometry.indices[0] = 0;
    invalid.draws[0].masks = vec![99];
    assert!(gpu.prepare_meshes(&invalid, &images).is_err());
    invalid.draws[0].masks.clear();
    invalid.order = vec![8];
    assert!(gpu.prepare_meshes(&invalid, &images).is_err());
    let cold = Batch {
        draws: vec![quad(&gpu, Texture::Pixels(color(&gpu, [1; 4])), -1., 1.)],
        order: vec![0],
        clear: Some([1.; 4]),
    };
    let resident = gpu.resident.clone();
    gpu.resident = Budget::new(0);
    assert!(gpu.prepare_meshes(&cold, &images).is_err());
    gpu.resident = resident;
    assert_eq!(read(&gpu, &images[&reference.id]), before);
}
