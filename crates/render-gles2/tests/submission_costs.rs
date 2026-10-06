#![cfg(target_os = "linux")]
mod support;
use glow::HasContext;
use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, ImageRef, Node, Scene, Size},
    mesh,
    pixels::{Bytes, Pixels},
    transition::{Effect, Frame, SceneTransition},
};
use krkr_render_gles2::{Config, Gpu, SceneState};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::HashMap,
    ffi::c_void,
    sync::Arc,
    time::Instant,
};

struct Counting;
#[global_allocator]
static ALLOCATOR: Counting = Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}
fn allocated() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        ALLOCS.set(ALLOCS.get() + 1);
    }
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

type Pointer = unsafe extern "system" fn(u32, i32, u32, u8, i32, *const c_void);
type Parameter = unsafe extern "system" fn(u32, u32, i32);
type DrawElements = unsafe extern "system" fn(u32, i32, u32, *const c_void);
type GetError = unsafe extern "system" fn() -> u32;
thread_local! {
    static POINTER: Cell<Option<Pointer>> = const { Cell::new(None) };
    static PARAMETER: Cell<Option<Parameter>> = const { Cell::new(None) };
    static LAYOUTS: Cell<usize> = const { Cell::new(0) };
    static FILTERS: Cell<usize> = const { Cell::new(0) };
    static DRAW: Cell<Option<DrawElements>> = const { Cell::new(None) };
    static ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
}
unsafe extern "system" fn draw_elements(mode: u32, count: i32, ty: u32, indices: *const c_void) {
    if let Some(left) = FAIL_AFTER.get() {
        FAIL_AFTER.set(Some(left.saturating_sub(1)));
    }
    unsafe { DRAW.get().unwrap()(mode, count, ty, indices) };
}
unsafe extern "system" fn get_error() -> u32 {
    if FAIL_AFTER.get() == Some(0) {
        FAIL_AFTER.set(None);
        glow::INVALID_OPERATION
    } else {
        unsafe { ERROR.get().unwrap()() }
    }
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
unsafe extern "system" fn parameter(target: u32, name: u32, value: i32) {
    if matches!(name, glow::TEXTURE_MIN_FILTER | glow::TEXTURE_MAG_FILTER) {
        FILTERS.set(FILTERS.get() + 1);
    }
    unsafe { PARAMETER.get().unwrap()(target, name, value) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glDrawElements" => {
            DRAW.set(Some(unsafe {
                std::mem::transmute::<*const c_void, DrawElements>(address)
            }));
            draw_elements as *const c_void
        }
        "glGetError" => {
            ERROR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, GetError>(address)
            }));
            get_error as *const c_void
        }
        "glVertexAttribPointer" => {
            POINTER.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Pointer>(address)
            }));
            pointer as *const c_void
        }
        "glTexParameteri" => {
            PARAMETER.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Parameter>(address)
            }));
            parameter as *const c_void
        }
        _ => address,
    }
}

#[test]
fn repeated_mesh_atlas_preserves_pixels_and_restores_nearest_sampling() {
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
        let budget = Budget::new(1024 * 1024);
        let size = Size {
            width: 64,
            height: 64,
        };
        let mut data = Bytes::zeroed(4 * 4 * 4, &budget).unwrap();
        for (i, p) in data
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            *p = [i as u8 * 13, 200 - i as u8 * 7, 37 + i as u8 * 11, 173];
        }
        let atlas = Arc::new(Pixels {
            size: Size {
                width: 4,
                height: 4,
            },
            main: Some(data),
            province: None,
        });
        let mut batch = mesh::Batch::default();
        for i in 0..48 {
            batch.draws.push(mesh::Draw {
                geometry: mesh::Geometry {
                    vertices: [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]]
                        .into_iter()
                        .map(|position| mesh::Vertex {
                            position,
                            uv: position.map(|v| (v + 1.) * 0.5),
                        })
                        .collect(),
                    indices: vec![0, 1, 2, 2, 3, 0],
                    _permit: budget.reserve(4 * 16 + 6 * 2).unwrap(),
                },
                texture: mesh::Texture::Pixels(atlas.clone()),
                blend: mesh::Blend::Alpha,
                opacity: 0.75,
                color: [1. - i as f32 / 96., 0.8, 0.9, 1.],
                solid_color: false,
                masks: vec![],
                visible: true,
            });
        }
        let images = HashMap::new();
        let mut expected = gpu.create_image(size, 0xff123456).unwrap();
        for i in 0..48 {
            batch.order = vec![i];
            let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
            gpu.draw_meshes(&mut expected, prepared).unwrap();
        }
        let expected = gpu.readback(&expected, size.rect(), false).unwrap();
        batch.order = (0..48).collect();
        let mut actual = gpu.create_image(size, 0xff123456).unwrap();
        let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
        LAYOUTS.set(0);
        FILTERS.set(0);
        gpu.draw_meshes(&mut actual, prepared).unwrap();
        let (layouts, filters) = (LAYOUTS.get(), FILTERS.get());
        eprintln!("mesh work={work} draws=48 position_layouts={layouts} filter_changes={filters}");
        assert_eq!(
            layouts, 48,
            "mesh draws must not configure a discarded quad"
        );
        assert_eq!(filters, 4, "one atlas needs one linear/nearest filter pair");
        unsafe {
            raw.active_texture(glow::TEXTURE0);
            assert_eq!(
                raw.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER),
                glow::NEAREST as i32
            );
            assert_eq!(
                raw.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER),
                glow::NEAREST as i32
            );
        }
        let actual = gpu.readback(&actual, size.rect(), false).unwrap();
        for (i, (a, b)) in actual
            .data
            .as_slice()
            .iter()
            .zip(expected.data.as_slice())
            .enumerate()
        {
            assert_eq!(a, b, "work={work} byte={i}");
        }
        // Error after the second draw must also restore the shared texture's
        // filter, so later sprite/scene rendering cannot inherit linear mode.
        let mut failed = gpu.create_image(size, 0).unwrap();
        let prepared = gpu.prepare_meshes(&batch, &images).unwrap();
        FAIL_AFTER.set(Some(2));
        assert!(gpu.draw_meshes(&mut failed, prepared).is_err());
        assert_eq!(FAIL_AFTER.get(), None);
        unsafe {
            raw.active_texture(glow::TEXTURE0);
            assert_eq!(
                raw.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER),
                glow::NEAREST as i32
            );
            assert_eq!(
                raw.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER),
                glow::NEAREST as i32
            );
        }
    }
}

fn node(size: Size) -> Node {
    Node {
        parent: None,
        cache: None,
        visible: true,
        opacity: 255,
        image: None,
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        neutral_color: 0xff123456,
    }
}

#[test]
fn retained_single_tile_versions_do_not_allocate_per_node() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut keys = slotmap::SlotMap::with_key();
    let id = keys.insert(());
    let reference = ImageRef {
        id,
        lifetime: Arc::default(),
    };
    let images = HashMap::from([(id, gpu.create_image(size, 0xff123456).unwrap())]);
    let scene = Scene {
        nodes: (0..256)
            .map(|_| Node {
                image: Some(reference.clone()),
                ..node(size)
            })
            .collect(),
        ..Default::default()
    };
    let mut state = SceneState::default();
    let mut canvas = None;
    gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
        .unwrap();
    ALLOCS.set(0);
    COUNTING.set(true);
    let result = gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images);
    COUNTING.set(false);
    let allocations = ALLOCS.get();
    assert_eq!(result.unwrap(), None);
    eprintln!("retained nodes=256 allocations={allocations}");
    assert!(
        allocations <= 4,
        "per-node allocation returned: {allocations}"
    );
}

#[test]
#[ignore = "CPU timing probe; run explicitly in release"]
fn retained_transition_membership_cost() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 16,
        height: 16,
    };
    for (nodes, effects) in [(512, 8), (2048, 64)] {
        let mut scene = Scene {
            nodes: (0..nodes)
                .map(|_| Node {
                    visible: false,
                    ..node(size)
                })
                .collect(),
            ..Default::default()
        };
        for i in 0..effects {
            scene.transitions.push(SceneTransition {
                destination: i * 2,
                source: i * 2 + 1,
                with_children: true,
                frame: Frame {
                    effect: Effect::CrossFade,
                    face: krkr_protocol::graphics::DrawFace::Alpha,
                    size,
                    phase: 127,
                },
                rule: None,
                custom: None,
            });
        }
        let images = HashMap::new();
        let mut state = SceneState::default();
        let mut canvas = None;
        gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
            .unwrap();
        let mut samples = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            for _ in 0..100 {
                assert_eq!(
                    gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
                        .unwrap(),
                    None
                );
            }
            samples.push(start.elapsed().as_nanos() / 100);
        }
        samples.sort_unstable();
        eprintln!(
            "retained nodes={nodes} transitions={effects} median_ns={}",
            samples[4]
        );
    }
}
