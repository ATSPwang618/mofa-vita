use super::*;
use krkr_protocol::{
    budget::Budget,
    graphics::{ImageLifetime, ImageRef, Size},
    mesh::{Draw, Geometry, Vertex},
    pixels::Bytes,
};
use std::time::{Duration, Instant};

fn quad(budget: &Budget, texture: Texture, left: f32, right: f32) -> Draw {
    let permit = budget.reserve(4 * 16 + 6 * 2).unwrap();
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
            _permit: permit,
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
fn asset(budget: &Budget, color: [u8; 4]) -> Arc<Pixels> {
    let mut bytes = Bytes::zeroed(4, budget).unwrap();
    bytes.as_mut_slice().copy_from_slice(&color);
    Arc::new(Pixels {
        size: Size {
            width: 1,
            height: 1,
        },
        main: Some(bytes),
        province: None,
    })
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn native_canvas_masks_blends_residency_and_snapshot_ownership() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let size = Size {
        width: 960,
        height: 544,
    };
    let canvas_bytes = size.rgba_bytes().unwrap();
    gpu.resident = Budget::new(canvas_bytes * 2 + 64);
    gpu.scratch = Budget::new(canvas_bytes);
    let budget = Budget::new(canvas_bytes * 2);
    let red = asset(&budget, [255, 0, 0, 255]);
    let white = asset(&budget, [255; 4]);
    let mut left_mask = quad(&budget, Texture::Pixels(white.clone()), -1., 0.);
    left_mask.opacity = 0.5;
    let mut right_mask = quad(&budget, Texture::Pixels(white.clone()), 0., 1.);
    right_mask.opacity = 0.49;
    let mut left = quad(&budget, Texture::Pixels(red.clone()), -1., 1.);
    left.masks = vec![0];
    let mut right = quad(&budget, Texture::Pixels(red.clone()), -1., 1.);
    right.masks = vec![1];
    let batch = Batch {
        draws: vec![left_mask, right_mask, left, right],
        order: vec![2, 3],
        clear: Some([0.; 4]),
    };
    let images = HashMap::new();
    let mut target = gpu.create_image(size, 0).unwrap();
    let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
    let resident_textures = prepared
        .textures
        .iter()
        .map(|i| i.main().unwrap().clone())
        .collect::<Vec<_>>();
    gpu.draw_meshes(&mut target, &batch, prepared).unwrap();
    // Submit another frame before waiting: a tight single-mask budget must
    // work without allocating one mask per draw or one per in-flight frame.
    let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
    for (old, new) in resident_textures.iter().zip(&prepared.textures) {
        assert!(
            Arc::ptr_eq(old, new.main().unwrap()),
            "immutable asset was uploaded again"
        );
    }
    gpu.draw_meshes(&mut target, &batch, prepared).unwrap();
    for (index, pixel) in read(&gpu, &target).as_chunks::<4>().0.iter().enumerate() {
        let expected = if index % 960 < 480 {
            [255, 0, 0, 255]
        } else {
            [0; 4]
        };
        assert_eq!(*pixel, expected, "mask/triangle seam at {index}");
    }
    assert_eq!(gpu.scratch.used(), canvas_bytes);
    assert_eq!(gpu.resident.used(), canvas_bytes + 8);

    // Small analytic GL reference blend cases, including solid color and the
    // Layer-manager /256 alpha quantization. These exercise actual shaders.
    let color = asset(&budget, [204, 102, 51, 128]);
    let mut tiny = gpu
        .create_image(
            Size {
                width: 1,
                height: 1,
            },
            0,
        )
        .unwrap();
    for (blend, expected) in [
        (Blend::AlphaMax, [70, 64, 70, 102]),
        (Blend::MultiplyAdd, [46, 71, 92, 102]),
        (Blend::Alpha, [70, 64, 70, 92]),
        (Blend::LayerAlpha, [69, 64, 70, 92]),
    ] {
        let mut draw = quad(&budget, Texture::Pixels(color.clone()), -1., 1.);
        draw.opacity = 0.5;
        draw.blend = blend;
        let batch = Batch {
            draws: vec![draw],
            order: vec![0],
            clear: Some([0.1, 0.2, 0.3, 0.4]),
        };
        let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
        gpu.draw_meshes(&mut tiny, &batch, prepared).unwrap();
        for (a, b) in read(&gpu, &tiny).iter().zip(expected) {
            assert!(a.abs_diff(b) <= 1, "{blend:?}: {a} != {b}");
        }
    }
    let mut draw = quad(&budget, Texture::Pixels(color), -1., 1.);
    draw.solid_color = true;
    draw.color = [0., 1., 0., 1.];
    draw.blend = Blend::Alpha;
    let solid = Batch {
        draws: vec![draw],
        order: vec![0],
        clear: Some([0.; 4]),
    };
    let prepared = gpu.prepare_meshes(&solid, &images).unwrap();
    gpu.draw_meshes(&mut tiny, &solid, prepared).unwrap();
    assert_eq!(read(&gpu, &tiny), [0, 128, 0, 64]);

    // Image sources hold logical owners until submission: reading and writing
    // the same image must preserve the old source instead of sampling a target.
    let mut ids = slotmap::SlotMap::<ImageId, ()>::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::new(ImageLifetime::default()),
    };
    let mut images = HashMap::from([(reference.id, tiny)]);
    let alias = Batch {
        draws: vec![quad(&budget, Texture::Image(reference.clone()), -1., 1.)],
        order: vec![0],
        clear: Some([0.; 4]),
    };
    let before = read(&gpu, &images[&reference.id]);
    let prepared = gpu.prepare_meshes(&alias, &images).unwrap();
    gpu.draw_meshes(images.get_mut(&reference.id).unwrap(), &alias, prepared)
        .unwrap();
    assert_eq!(read(&gpu, &images[&reference.id]), [0, 32, 0, 64]);
    assert_eq!(before, [0, 128, 0, 64]);
    // Exhaust fresh allocations after releasing reusable slots. Reusing an
    // already charged texture is allowed even when the new budget is empty.
    gpu.collect_mesh_textures();
    gpu.trim_resident_pool();
    let empty_budget = std::mem::replace(&mut gpu.resident, Budget::new(0));
    let missing = Batch {
        draws: vec![quad(
            &budget,
            Texture::Pixels(asset(&budget, [1; 4])),
            -1.,
            1.,
        )],
        order: vec![0],
        clear: None,
    };
    assert!(gpu.prepare_meshes(&missing, &images).is_err());
    gpu.resident = empty_budget;
    assert_eq!(read(&gpu, &images[&reference.id]), [0, 32, 0, 64]);
}
