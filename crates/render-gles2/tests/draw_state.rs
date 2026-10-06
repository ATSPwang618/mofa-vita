#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use glow::HasContext;
use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, ImageRef, Node, Rect, Scene, Size};
use krkr_protocol::{
    pixels::{Bytes, Pixels},
    transform::{Filter, ImageOperation, Sampling, Transform},
};
use krkr_render_gles2::{Config, Gpu};
use std::{cell::Cell, collections::HashMap, ffi::c_void, sync::Arc};

type Use = unsafe extern "system" fn(u32);
type Pointer = unsafe extern "system" fn(u32, i32, u32, u8, i32, *const c_void);
thread_local! {
    static USE: Cell<Option<Use>> = const { Cell::new(None) };
    static POINTER: Cell<Option<Pointer>> = const { Cell::new(None) };
    static BINDS: Cell<usize> = const { Cell::new(0) };
    static LAYOUTS: Cell<usize> = const { Cell::new(0) };
}
unsafe extern "system" fn use_program(program: u32) {
    BINDS.set(BINDS.get() + 1);
    unsafe { USE.get().unwrap()(program) };
}
unsafe extern "system" fn pointer(
    i: u32,
    n: i32,
    ty: u32,
    normalized: u8,
    stride: i32,
    offset: *const c_void,
) {
    if i == 0 {
        LAYOUTS.set(LAYOUTS.get() + 1);
    }
    unsafe { POINTER.get().unwrap()(i, n, ty, normalized, stride, offset) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glUseProgram" => {
            USE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Use>(address)
            }));
            use_program as *const c_void
        }
        "glVertexAttribPointer" => {
            POINTER.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Pointer>(address)
            }));
            pointer as *const c_void
        }
        _ => address,
    }
}

#[test]
fn composition_reuses_draw_state_but_rebinds_after_external_changes() {
    for work in [false, true] {
        let context = support::Context::new();
        let raw = context.gl();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 128,
            height: 96,
        };
        let small = Size {
            width: 18,
            height: 15,
        };
        let mut ids = slotmap::SlotMap::with_key();
        let mut images = HashMap::new();
        let mut scene = Scene::default();
        scene.nodes.push(Node {
            parent: None,
            cache: None,
            visible: true,
            opacity: 255,
            image: None,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            neutral_color: 0xff223344,
        });
        for i in 0..48 {
            let id = ids.insert(());
            let mut image = gpu
                .create_image(small, 0x71413753 + i * 0x01020301)
                .unwrap();
            gpu.color(
                &mut image,
                Rect {
                    left: 2,
                    top: 2,
                    width: 9,
                    height: 7,
                },
                0x00807060,
                93,
                DrawFace::Alpha,
            )
            .unwrap();
            images.insert(id, image);
            scene.nodes.push(Node {
                parent: Some(0),
                cache: None,
                visible: true,
                opacity: 193,
                image: Some(ImageRef {
                    id,
                    lifetime: Arc::default(),
                }),
                rectangle: Rect {
                    left: (i % 8 * 14) as i32,
                    top: (i / 8 * 12) as i32,
                    ..small.rect()
                },
                image_left: 0,
                image_top: 0,
                blend: Blend::AddAlpha,
                neutral_color: 0,
            });
        }
        // Warm the programs. The measured frame still draws every leaf.
        drop(gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap());
        for varied in [false, true] {
            for (i, node) in scene.nodes.iter_mut().enumerate().skip(1) {
                if varied && i % 3 == 0 {
                    node.blend = Blend::Multiplicative;
                }
            }
            let mut expected = gpu.create_image(size, 0xff223344).unwrap();
            for node in scene.nodes.iter().skip(1) {
                gpu.operate(
                    &mut expected,
                    &images[&node.image.as_ref().unwrap().id],
                    small.rect(),
                    node.rectangle.left,
                    node.rectangle.top,
                    size.rect(),
                    BlendOptions::for_composition(node.blend, DrawFace::Opaque, node.opacity),
                )
                .unwrap();
            }
            let expected = gpu
                .readback(&expected, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec();
            // Fail after drawing most of the scene. Both pending work-surface
            // pixels and scoped bindings must recover on the next call.
            let missing = scene.nodes.last().unwrap().image.as_ref().unwrap().id;
            let saved = images.remove(&missing).unwrap();
            let failed = gpu.scene_surface(size, &scene, &images, (0, 0));
            images.insert(missing, saved);
            assert!(failed.is_err());
            // Sharing a context between calls is permitted. No state from the
            // preceding composition may be trusted at the next public entry.
            unsafe {
                raw.use_program(None);
                raw.disable_vertex_attrib_array(0);
                raw.enable_vertex_attrib_array(1);
                raw.bind_buffer(glow::ARRAY_BUFFER, None);
            }
            BINDS.set(0);
            LAYOUTS.set(0);
            let actual = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
            let (binds, layouts) = (BINDS.get(), LAYOUTS.get());
            eprintln!("work={work} varied={varied} program_binds={binds} quad_layouts={layouts}");
            assert_eq!(layouts, 1, "quad geometry is unchanged across the scene");
            if !varied {
                assert!(
                    binds <= if work { 4 } else { 2 },
                    "redundant program binds: {binds}"
                );
            }
            let actual = gpu.readback(&actual, size.rect(), false).unwrap();
            assert_eq!(actual.data.as_slice().len(), expected.len());
            for (pixel, (actual, expected)) in actual
                .data
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0)
                .enumerate()
            {
                assert_eq!(
                    actual, expected,
                    "work={work} varied={varied} pixel={pixel}"
                );
            }
        }
    }
}

#[test]
fn tiled_affine_reuses_quad_state_and_rebinds_on_the_next_call() {
    for work in [false, true] {
        let context = support::Context::new();
        let raw = context.gl();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(intercept),
                Config {
                    work_framebuffer: work,
                    tile_edge: 64,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let source_size = Size {
            width: 256,
            height: 160,
        };
        let output = Size {
            width: 128,
            height: 96,
        };
        let mut data = Bytes::zeroed(source_size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in data
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            pixel.copy_from_slice(&[(i * 31) as u8, (i * 17) as u8, (i * 73) as u8, 255]);
        }
        let source = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: source_size,
                    main: Some(data),
                    province: None,
                },
            )
            .unwrap();
        let transform = Transform::Affine([[16.25, -2.5], [138.25, 17.5], [-3.75, 79.5]]);
        for filter in [Filter::Nearest, Filter::FastLinear] {
            let sampling = Sampling {
                filter,
                sharpness: -1.,
                no_clip: false,
            };
            let mut expected = gpu.create_image(output, 0xff123456).unwrap();
            gpu.transform(
                &mut expected,
                &source,
                source_size.rect(),
                transform,
                sampling,
                ImageOperation::Copy { hold_alpha: false },
                output.rect(),
                None,
            )
            .unwrap();
            let expected = gpu.readback(&expected, output.rect(), false).unwrap().data;
            let mut actual = gpu.create_image(output, 0xff123456).unwrap();
            gpu.resolve().unwrap();
            unsafe {
                raw.use_program(None);
                raw.disable_vertex_attrib_array(0);
                raw.enable_vertex_attrib_array(1);
                raw.bind_buffer(glow::ARRAY_BUFFER, None);
            }
            BINDS.set(0);
            LAYOUTS.set(0);
            gpu.transform(
                &mut actual,
                &source,
                source_size.rect(),
                transform,
                sampling,
                ImageOperation::Copy { hold_alpha: false },
                output.rect(),
                None,
            )
            .unwrap();
            eprintln!(
                "affine work={work} filter={filter:?} program_binds={} quad_layouts={}",
                BINDS.get(),
                LAYOUTS.get()
            );
            assert_eq!(LAYOUTS.get(), 1);
            let actual = gpu.readback(&actual, output.rect(), false).unwrap();
            assert_eq!(actual.data.as_slice(), expected.as_slice());
        }
    }
}
