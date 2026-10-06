#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
    text::{Glyph, PlacedGlyph, Run, Style},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    ffi::c_void,
    sync::Arc,
};

type Gen = unsafe extern "system" fn(i32, *mut u32);
type Delete = unsafe extern "system" fn(i32, *const u32);
type Status = unsafe extern "system" fn(u32) -> u32;
type GetError = unsafe extern "system" fn() -> u32;
type Bind = unsafe extern "system" fn(u32, u32);
type Attach = unsafe extern "system" fn(u32, u32, u32, u32, i32);
thread_local! {
    static GEN: Cell<Option<Gen>> = const { Cell::new(None) };
    static DELETE: Cell<Option<Delete>> = const { Cell::new(None) };
    static LIVE: Cell<i32> = const { Cell::new(0) };
    static PEAK: Cell<i32> = const { Cell::new(0) };
    static STATUS: Cell<Option<Status>> = const { Cell::new(None) };
    static GET_ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static FAIL: Cell<bool> = const { Cell::new(false) };
    static ERROR: Cell<u32> = const { Cell::new(0) };
    static BIND: Cell<Option<Bind>> = const { Cell::new(None) };
    static ATTACH: Cell<Option<Attach>> = const { Cell::new(None) };
    static DELETE_TEXTURES: Cell<Option<Delete>> = const { Cell::new(None) };
    static BOUND: Cell<u32> = const { Cell::new(0) };
    static ATTACHMENTS: RefCell<HashMap<u32, u32>> = RefCell::new(HashMap::new());
    // PVR owns the render surface on the mip level, not on the GL FBO.
    // Deleting an FBO leaves this quota charged until its texture is deleted.
    static SURFACES: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static SURFACE_LIMIT: Cell<usize> = const { Cell::new(usize::MAX) };
    static SURFACE_PEAK: Cell<usize> = const { Cell::new(0) };
}
fn hook(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glBindFramebuffer" => {
            BIND.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Bind>(address)
            }));
            bind_framebuffer as *const c_void
        }
        "glFramebufferTexture2D" => {
            ATTACH.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Attach>(address)
            }));
            attach_texture as *const c_void
        }
        "glDeleteTextures" => {
            DELETE_TEXTURES.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Delete>(address)
            }));
            delete_textures as *const c_void
        }
        "glGenFramebuffers" => {
            GEN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Gen>(address)
            }));
            gen_framebuffers as *const c_void
        }
        "glDeleteFramebuffers" => {
            DELETE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Delete>(address)
            }));
            delete_framebuffers as *const c_void
        }
        "glCheckFramebufferStatus" => {
            STATUS.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Status>(address)
            }));
            status as *const c_void
        }
        "glGetError" => {
            GET_ERROR.set(Some(unsafe {
                std::mem::transmute::<*const c_void, GetError>(address)
            }));
            get_error as *const c_void
        }
        _ => traffic::intercept(name, address),
    }
}
unsafe extern "system" fn status(target: u32) -> u32 {
    let result = unsafe { STATUS.get().unwrap()(target) };
    if FAIL.replace(false) {
        ERROR.set(glow::OUT_OF_MEMORY);
        return result;
    }
    if result == glow::FRAMEBUFFER_COMPLETE
        && let Some(texture) = ATTACHMENTS.with_borrow(|a| a.get(&BOUND.get()).copied())
    {
        let full = SURFACES.with_borrow_mut(|surfaces| {
            if !surfaces.contains(&texture) && surfaces.len() >= SURFACE_LIMIT.get() {
                return true;
            }
            surfaces.insert(texture);
            SURFACE_PEAK.set(SURFACE_PEAK.get().max(surfaces.len()));
            false
        });
        if full {
            ERROR.set(glow::OUT_OF_MEMORY);
            return glow::FRAMEBUFFER_UNSUPPORTED;
        }
    }
    result
}
unsafe extern "system" fn bind_framebuffer(target: u32, name: u32) {
    unsafe { BIND.get().unwrap()(target, name) };
    BOUND.set(name);
}
unsafe extern "system" fn attach_texture(
    target: u32,
    attachment: u32,
    kind: u32,
    texture: u32,
    level: i32,
) {
    unsafe { ATTACH.get().unwrap()(target, attachment, kind, texture, level) };
    ATTACHMENTS.with_borrow_mut(|a| {
        if texture == 0 {
            a.remove(&BOUND.get());
        } else {
            a.insert(BOUND.get(), texture);
        }
    });
}
unsafe extern "system" fn delete_textures(count: i32, names: *const u32) {
    unsafe { DELETE_TEXTURES.get().unwrap()(count, names) };
    for name in unsafe { std::slice::from_raw_parts(names, count as usize) } {
        SURFACES.with_borrow_mut(|s| s.remove(name));
    }
}
unsafe extern "system" fn get_error() -> u32 {
    let error = ERROR.replace(0);
    if error == 0 {
        unsafe { GET_ERROR.get().unwrap()() }
    } else {
        error
    }
}
unsafe extern "system" fn gen_framebuffers(count: i32, names: *mut u32) {
    unsafe { GEN.get().unwrap()(count, names) };
    LIVE.set(LIVE.get() + count);
    PEAK.set(PEAK.get().max(LIVE.get()));
}
unsafe extern "system" fn delete_framebuffers(count: i32, names: *const u32) {
    unsafe { DELETE.get().unwrap()(count, names) };
    for name in unsafe { std::slice::from_raw_parts(names, count as usize) } {
        ATTACHMENTS.with_borrow_mut(|a| a.remove(name));
        if BOUND.get() == *name {
            BOUND.set(0);
        }
    }
    LIVE.set(LIVE.get() - count);
}
const SIZE: Size = Size {
    width: 64,
    height: 64,
};
const TILE_BYTES: usize = 64 * 64 * 4;
fn gpu(context: &support::Context, entries: usize, bytes: usize) -> Gpu {
    assert_eq!(LIVE.get(), 0);
    SURFACES.with_borrow(|s| assert!(s.is_empty()));
    SURFACE_LIMIT.set(usize::MAX);
    SURFACE_PEAK.set(0);
    PEAK.set(0);
    unsafe {
        Gpu::new(
            context.gl_with(hook),
            Config {
                work_framebuffer: true,
                render_target_cache_entries: entries,
                render_target_cache_bytes: bytes,
                tile_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    }
}
fn image(gpu: &Gpu, seed: u8) -> Image {
    let mut bytes = Bytes::zeroed(TILE_BYTES, &Budget::new(TILE_BYTES)).unwrap();
    for (index, pixel) in bytes
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[(index % 233) as u8, seed, (index / 64) as u8, 173]);
    }
    gpu.upload_scaled(
        &Pixels {
            size: SIZE,
            main: Some(bytes),
            province: None,
        },
        SIZE,
    )
    .unwrap()
}
fn paint(gpu: &Gpu, image: &mut Image, value: u32) {
    gpu.fill(
        image,
        &[Fill {
            rectangle: Rect {
                left: 3,
                top: 11,
                width: 23,
                height: 13,
            },
            color: 0x9f000000 | value,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, SIZE.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn live_texture_surfaces_stay_bounded_across_target_churn_and_collection() {
    let context = support::Context::new();
    let gpu = gpu(&context, 2, 2 * TILE_BYTES);
    SURFACE_LIMIT.set(2);
    let mut images: Vec<_> = (0..24).map(|i| image(&gpu, i)).collect();
    for frame in 0..8 {
        for (i, image) in images.iter_mut().enumerate() {
            paint(&gpu, image, frame * 97 + i as u32);
        }
        gpu.resolve().unwrap();
        gpu.collect_under_pressure().unwrap();
    }
    // Half the cache remains reserved for scratch composition targets.
    assert_eq!(SURFACE_PEAK.get(), 1);
    assert_eq!(SURFACES.with_borrow(HashSet::len), 1);
    for (i, image) in images.iter().enumerate() {
        let pixels = read(&gpu, image);
        let color = 7 * 97 + i as u32;
        assert_eq!(
            &pixels[(11 * 64 + 3) * 4..(11 * 64 + 3) * 4 + 4],
            &[0, (color >> 8) as u8, color as u8, 0x9f]
        );
    }
    drop(images);
    gpu.collect_under_pressure().unwrap();
    assert_eq!(SURFACES.with_borrow(HashSet::len), 0);
    // Freed texture storage returns the slots for new scenes.
    let mut replacements: Vec<_> = (0..4).map(|i| image(&gpu, i)).collect();
    for _ in 0..3 {
        for image in &mut replacements {
            paint(&gpu, image, 0x334455);
        }
    }
    assert_eq!(SURFACES.with_borrow(HashSet::len), 1);
    drop((replacements, gpu));
    SURFACES.with_borrow(|s| assert!(s.is_empty()));
}

#[test]
fn alternating_canvases_remove_work_surface_transfers_after_warmup() {
    let context = support::Context::new();
    let mut reference = Vec::new();
    let mut baseline = 0;
    let mut baseline_loads = 0;
    for entries in [0, 8] {
        let gpu = gpu(&context, entries, 8 * TILE_BYTES);
        let mut images: Vec<_> = (0..6).map(|i| image(&gpu, i)).collect();
        for _ in 0..3 {
            for (index, image) in images.iter_mut().enumerate() {
                paint(&gpu, image, index as u32 + 1);
            }
        }
        gpu.resolve().unwrap();
        traffic::reset();
        for frame in 0..12 {
            for (index, image) in images.iter_mut().enumerate() {
                paint(&gpu, image, 0x20100 + frame * 7 + index as u32);
            }
            gpu.resolve().unwrap();
        }
        let stores = traffic::stored_pixels();
        let loads = traffic::loaded_pixels();
        let output: Vec<_> = images.iter().map(|image| read(&gpu, image)).collect();
        if entries == 0 {
            baseline = stores;
            baseline_loads = loads;
            reference = output;
        } else {
            assert_eq!(output, reference);
            // Four resident slots serve six canvases; the remaining four
            // slots are reserved for scratch, so two canvases still store.
            assert!(stores * 2 < baseline, "cached={stores} baseline={baseline}");
            assert!(
                loads < baseline_loads,
                "cached={loads}, baseline={baseline_loads}"
            );
            assert!(
                PEAK.get() >= 5,
                "more than three cached targets must be exercised"
            );
            println!("work pixels: store {baseline}->{stores}, load {baseline_loads}->{loads}");
        }
        drop((images, gpu));
        assert_eq!(LIVE.get(), 0);
    }
}

#[test]
fn admission_obeys_entry_and_byte_limits_without_losing_pixels() {
    let context = support::Context::new();
    let mut reference = Vec::new();
    for (entries, bytes) in [(0, 0), (2, 8 * TILE_BYTES), (8, 2 * TILE_BYTES)] {
        let gpu = gpu(&context, entries, bytes);
        let mut images: Vec<_> = (0..8).map(|i| image(&gpu, i)).collect();
        for frame in 0..6 {
            for (index, image) in images.iter_mut().enumerate() {
                paint(&gpu, image, 0x8000 + frame * 19 + index as u32);
            }
        }
        gpu.resolve().unwrap();
        let output: Vec<_> = images.iter().map(|image| read(&gpu, image)).collect();
        if entries == 0 {
            reference = output;
        } else {
            assert_eq!(output, reference);
            assert_eq!(
                PEAK.get(),
                2,
                "one work target plus one resident attachment; scratch quota stays free"
            );
        }
        gpu.collect_under_pressure().unwrap();
        assert_eq!(
            LIVE.get(),
            if entries == 0 { 1 } else { 2 },
            "live texture surfaces must retain their slots under pressure"
        );
        for (image, expected) in images.iter().zip(&reference) {
            assert_eq!(read(&gpu, image), *expected);
        }
        drop((images, gpu));
        assert_eq!(LIVE.get(), 0);
    }
}

#[test]
fn destination_reads_text_and_shared_snapshots_remain_pixel_exact() {
    let context = support::Context::new();
    let budget = Budget::new(64 * 1024);
    let mut mask = Bytes::zeroed(64, &budget).unwrap();
    for (i, byte) in mask.as_mut_slice().iter_mut().enumerate() {
        *byte = (i * 37 % 256) as u8;
    }
    let glyph = Arc::new(Glyph {
        id: 73021,
        size: Size {
            width: 8,
            height: 8,
        },
        origin: [0, 0],
        advance: [7, 0],
        levels: 256,
        mask,
    });
    let run = Run {
        glyphs: (0..6)
            .map(|i| PlacedGlyph {
                glyph: glyph.clone(),
                x: 3 + i * 7,
                y: 19,
                color: 0xd5b7f2,
            })
            .collect(),
        permit: budget.reserve(1024).unwrap(),
    };
    let mut reference = Vec::new();
    for entries in [0, 8] {
        let gpu = gpu(&context, entries, 8 * TILE_BYTES);
        let mut images: Vec<_> = (0..4).map(|i| image(&gpu, i)).collect();
        for _ in 0..3 {
            for (i, image) in images.iter_mut().enumerate() {
                paint(&gpu, image, i as u32);
            }
        }
        let snapshot = images[0].shared();
        let old = read(&gpu, &snapshot);
        for frame in 0..5 {
            for image in &mut images {
                gpu.color(
                    image,
                    Rect {
                        left: 5,
                        top: 9,
                        width: 41,
                        height: 32,
                    },
                    0x37658a,
                    91 + frame,
                    DrawFace::Alpha,
                )
                .unwrap();
                gpu.draw_text(
                    image,
                    &run,
                    Style {
                        color: 0xd5b7f2,
                        opacity: 197,
                        antialias: true,
                        shadow_level: 100,
                        shadow_color: 0x18283a,
                        shadow_width: 1,
                        shadow_offset: [1, 1],
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    },
                    SIZE.rect(),
                )
                .unwrap();
                paint(&gpu, image, 0x209e46 + frame as u32);
            }
            gpu.resolve().unwrap();
        }
        assert_eq!(read(&gpu, &snapshot), old);
        let output: Vec<_> = images.iter().map(|image| read(&gpu, image)).collect();
        if entries == 0 {
            reference = output;
        } else {
            assert_eq!(output, reference);
        }
        drop((snapshot, images, gpu));
        assert_eq!(LIVE.get(), 0);
    }
}

#[test]
fn recycled_attached_textures_clear_and_upload_without_recreating_fbos() {
    let context = support::Context::new();
    let gpu = gpu(&context, 8, 8 * TILE_BYTES);
    let mut images: Vec<_> = (0..4).map(|i| image(&gpu, i)).collect();
    for _ in 0..3 {
        for (i, image) in images.iter_mut().enumerate() {
            paint(&gpu, image, i as u32);
        }
    }
    gpu.resolve().unwrap();
    let live = LIVE.get();
    assert!(live > 1);
    drop(images);
    gpu.maintain().unwrap();
    let blank = gpu.reserve_upload(SIZE, true, false).unwrap();
    assert_eq!(read(&gpu, &blank), vec![0; TILE_BYTES]);
    assert_eq!(LIVE.get(), live);
    drop(blank);
    let loaded = image(&gpu, 211);
    assert_eq!(&read(&gpu, &loaded)[..4], &[0, 211, 0, 173]);
    drop(loaded);
    gpu.collect().unwrap();
    assert_eq!(LIVE.get(), 1);
    drop(gpu);
    assert_eq!(LIVE.get(), 0);
}

#[test]
fn attachment_oom_preserves_live_target_slots_when_retrying_the_undrawn_pass() {
    let context = support::Context::new();
    let gpu = gpu(&context, 8, 8 * TILE_BYTES);
    let mut images: Vec<_> = (0..4).map(|i| image(&gpu, i)).collect();
    for _ in 0..3 {
        for (i, image) in images.iter_mut().enumerate() {
            paint(&gpu, image, i as u32);
        }
    }
    gpu.resolve().unwrap();
    assert!(LIVE.get() > 1);
    let expected: Vec<_> = images.iter().map(|image| read(&gpu, image)).collect();
    let mut next = image(&gpu, 177);
    paint(&gpu, &mut next, 0x559177);
    // Move the work surface away so the next write can promote this canvas.
    let mut other = image(&gpu, 99);
    paint(&gpu, &mut other, 0x419785);
    let before = LIVE.get();
    FAIL.set(true);
    paint(&gpu, &mut next, 0x349157);
    assert!(
        !FAIL.get(),
        "the attachment must exercise the injected failure"
    );
    assert_eq!(
        LIVE.get(),
        before + 1,
        "live surfaces plus the retried target"
    );
    let actual = read(&gpu, &next);
    assert_eq!(
        &actual[(11 * 64 + 3) * 4..(11 * 64 + 3) * 4 + 4],
        &[0x34, 0x91, 0x57, 0x9f]
    );
    for (image, old) in images.iter().zip(&expected) {
        assert_eq!(read(&gpu, image), *old);
    }
    drop((next, other, images, gpu));
    assert_eq!(LIVE.get(), 0);
}

#[test]
fn transitions_and_cropped_copies_match_the_work_surface_with_cached_targets() {
    use krkr_protocol::transition::{Direction, Effect, Stay};

    let context = support::Context::new();
    let mut reference = Vec::new();
    for entries in [0, 8] {
        let gpu = gpu(&context, entries, 8 * TILE_BYTES);
        let first = image(&gpu, 31);
        let second = image(&gpu, 191);
        let rule = image(&gpu, 57);
        let mut outputs: Vec<_> = (0..3).map(|i| image(&gpu, i)).collect();
        for _ in 0..3 {
            for (i, output) in outputs.iter_mut().enumerate() {
                paint(&gpu, output, i as u32);
            }
        }
        let mut snapshots = Vec::new();
        for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
            for effect in [
                Effect::CrossFade,
                Effect::Universal { vague: 64 },
                Effect::Scroll {
                    from: Direction::Left,
                    stay: Stay::Source,
                },
            ] {
                for phase in [41, 113, 229] {
                    for output in &mut outputs {
                        gpu.transition(
                            output,
                            &first,
                            &second,
                            Some(&rule),
                            effect.frame(face, SIZE, phase, 255),
                        )
                        .unwrap();
                    }
                    let source = outputs[0].shared();
                    gpu.copy_rect(
                        &mut outputs[1],
                        &source,
                        Rect {
                            left: 7,
                            top: 5,
                            width: 41,
                            height: 27,
                        },
                        11,
                        19,
                        SIZE.rect(),
                        DrawFace::Alpha,
                        false,
                    )
                    .unwrap();
                    gpu.resolve().unwrap();
                    snapshots.extend(outputs.iter().map(|image| read(&gpu, image)));
                }
            }
        }
        if entries == 0 {
            reference = snapshots;
        } else {
            assert_eq!(snapshots, reference);
            assert!(PEAK.get() > 1);
        }
        drop((outputs, rule, first, second, gpu));
        assert_eq!(LIVE.get(), 0);
    }
}
