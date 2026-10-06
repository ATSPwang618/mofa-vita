#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;

use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Size},
    pixels::Bytes,
    texture::{Compressed, Format},
};
use krkr_render_gles2::{Config, Gpu};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
};

type CompressedImage = unsafe extern "system" fn(u32, i32, u32, i32, i32, i32, i32, *const c_void);
type Draw = unsafe extern "system" fn(u32, i32, i32);
type Get = unsafe extern "system" fn(u32, *mut i32);
type GetError = unsafe extern "system" fn() -> u32;
type Finish = unsafe extern "system" fn();
type Delete = unsafe extern "system" fn(i32, *const u32);
const MIB: usize = 1024 * 1024;

// Model PVR's ETC1/PVRTC host levels separately from texture payload permits.
// Finish alone cannot retire a level that has never been sampled. This is a
// deterministic backpressure model, not a measurement of Vita's cleanup delay.
struct Level {
    name: u32,
    bytes: usize,
    sampled: bool,
}
thread_local! {
    static IMAGE: Cell<Option<CompressedImage>> = const { Cell::new(None) };
    static DRAW: Cell<Option<Draw>> = const { Cell::new(None) };
    static GET: Cell<Option<Get>> = const { Cell::new(None) };
    static ERROR: Cell<Option<GetError>> = const { Cell::new(None) };
    static FINISH: Cell<Option<Finish>> = const { Cell::new(None) };
    static DELETE: Cell<Option<Delete>> = const { Cell::new(None) };
    static LEVELS: RefCell<Vec<Level>> = const { RefCell::new(Vec::new()) };
    static OOM: Cell<bool> = const { Cell::new(false) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
    static WAITS: Cell<usize> = const { Cell::new(0) };
    static DRAWS: Cell<usize> = const { Cell::new(0) };
}
fn hook(name: &str, address: *const c_void) -> *const c_void {
    macro_rules! intercept {
        ($slot:ident, $kind:ty, $function:ident) => {{
            $slot.set(Some(unsafe {
                std::mem::transmute::<*const c_void, $kind>(address)
            }));
            $function as *const c_void
        }};
    }
    match name {
        "glCompressedTexImage2D" => intercept!(IMAGE, CompressedImage, compressed_image),
        "glDrawArrays" => intercept!(DRAW, Draw, draw),
        "glGetIntegerv" => {
            GET.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Get>(address)
            }));
            address
        }
        "glGetError" => intercept!(ERROR, GetError, get_error),
        "glFinish" => intercept!(FINISH, Finish, finish),
        "glDeleteTextures" => intercept!(DELETE, Delete, delete),
        _ => address,
    }
}
unsafe fn bound() -> u32 {
    let mut name = 0;
    unsafe { GET.get().unwrap()(glow::TEXTURE_BINDING_2D, &mut name) };
    name as u32
}
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn compressed_image(
    target: u32,
    level: i32,
    format: u32,
    width: i32,
    height: i32,
    border: i32,
    bytes: i32,
    data: *const c_void,
) {
    if format != Format::Etc1.gl_internal() && format != Format::Pvrtc1Rgba4.gl_internal() {
        unsafe { IMAGE.get().unwrap()(target, level, format, width, height, border, bytes, data) };
        return;
    }
    let used = LEVELS.with_borrow(|levels| levels.iter().map(|l| l.bytes).sum::<usize>());
    if used + bytes as usize > 4 * MIB {
        OOM.set(true);
        return;
    }
    unsafe { IMAGE.get().unwrap()(target, level, format, width, height, border, bytes, data) };
    LEVELS.with_borrow_mut(|levels| {
        levels.push(Level {
            name: unsafe { bound() },
            bytes: bytes as usize,
            sampled: false,
        })
    });
    PEAK.set(PEAK.get().max(used + bytes as usize));
}
unsafe extern "system" fn draw(mode: u32, first: i32, count: i32) {
    DRAWS.set(DRAWS.get() + 1);
    let name = unsafe { bound() };
    LEVELS.with_borrow_mut(|levels| {
        for level in levels.iter_mut().filter(|l| l.name == name) {
            level.sampled = true;
        }
    });
    unsafe { DRAW.get().unwrap()(mode, first, count) };
}
unsafe extern "system" fn finish() {
    unsafe { FINISH.get().unwrap()() };
    LEVELS.with_borrow_mut(|levels| levels.retain(|l| !l.sampled));
    WAITS.set(WAITS.get() + 1);
}
unsafe extern "system" fn get_error() -> u32 {
    if OOM.replace(false) {
        glow::OUT_OF_MEMORY
    } else {
        unsafe { ERROR.get().unwrap()() }
    }
}
unsafe extern "system" fn delete(count: i32, names: *const u32) {
    if count > 0 {
        let names = unsafe { std::slice::from_raw_parts(names, count as usize) };
        LEVELS.with_borrow_mut(|levels| levels.retain(|l| !names.contains(&l.name)));
    }
    unsafe { DELETE.get().unwrap()(count, names) };
}
fn setup(context: &support::Context) -> Gpu {
    LEVELS.with_borrow_mut(Vec::clear);
    OOM.set(false);
    PEAK.set(0);
    WAITS.set(0);
    DRAWS.set(0);
    unsafe {
        Gpu::new(
            context.gl_with(hook),
            Config {
                work_framebuffer: true,
                tile_edge: 1024,
                ..Default::default()
            },
        )
        .unwrap()
    }
}
fn asset(size: Size, format: Format) -> Compressed {
    let tile = Size {
        width: 1024,
        height: 1024,
    };
    let length = Compressed::payload_len(size, tile, format).unwrap();
    let mut data = Bytes::zeroed(length, &Budget::new(length)).unwrap();
    if format.linear() == Format::Bc1Rgb {
        for block in data.as_mut_slice().as_chunks_mut::<8>().0 {
            block.copy_from_slice(&[0, 0xf8, 0, 0, 0, 0, 0, 0]);
        }
    }
    Compressed::tiled(size, tile, format, data, 0).unwrap()
}

#[test]
#[cfg_attr(windows, ignore = "ANGLE/D3D11 does not expose ETC1")]
fn hidden_etc_images_do_not_accumulate_unsampled_host_levels() {
    let context = support::Context::new();
    let gpu = setup(&context);
    let texture = asset(
        Size {
            width: 1024,
            height: 1024,
        },
        Format::Etc1,
    );
    assert!(gpu.supports_compressed(&texture));
    let baseline = gpu.resident.used();
    let mut hidden = Vec::new();
    // No scene render, swap, user input or explicit collection between loads.
    for _ in 0..24 {
        hidden.push(gpu.load_compressed(&texture).unwrap());
    }
    assert_eq!(gpu.resident.used() - baseline, 12 * MIB);
    assert!(LEVELS.with_borrow(Vec::is_empty));
    assert_eq!(gpu.pixel(&hidden[0], 4, 4, false).unwrap(), 0xff020202);
    drop(hidden);
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), baseline);
}

#[test]
#[cfg_attr(windows, ignore = "ANGLE/D3D11 does not expose ETC1")]
fn tiled_etc_upload_bounds_host_copies_and_preserves_pending_canvas_writes() {
    let context = support::Context::new();
    let gpu = setup(&context);
    let small = Size {
        width: 32,
        height: 32,
    };
    let mut canvas = gpu.create_image(small, 0).unwrap();
    gpu.fill(
        &mut canvas,
        &[Fill {
            rectangle: small.rect(),
            color: 0xff193c57,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let texture = asset(
        Size {
            width: 4096,
            height: 4096,
        },
        Format::Etc1,
    );
    assert!(gpu.supports_compressed(&texture));
    WAITS.set(0);
    let image = gpu.load_compressed(&texture).unwrap();
    assert!(PEAK.get() <= 4 * MIB);
    assert!(LEVELS.with_borrow(Vec::is_empty));
    assert!(WAITS.get() < 16, "do not wait separately for every tile");
    assert_eq!(image.resident_bytes(), 8 * MIB, "keep native ETC storage");
    assert_eq!(gpu.pixel(&canvas, 0, 0, false).unwrap(), 0xff193c57);
    assert_eq!(gpu.pixel(&canvas, 31, 31, false).unwrap(), 0xff193c57);
    assert_eq!(gpu.pixel(&image, 3073, 3073, false).unwrap(), 0xff020202);
}

#[test]
fn bc_uploads_preserve_pending_canvas_without_draws_or_waits() {
    let context = support::Context::new();
    for format in [Format::Bc1Rgb, Format::Bc1RgbVita] {
        let gpu = setup(&context);
        let size = Size {
            width: 32,
            height: 32,
        };
        let mut canvas = gpu.create_image(size, 0).unwrap();
        gpu.fill(
            &mut canvas,
            &[Fill {
                rectangle: size.rect(),
                color: 0xff193c57,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let texture = asset(
            Size {
                width: 4096,
                height: 4096,
            },
            format,
        );
        WAITS.set(0);
        DRAWS.set(0);
        let image = gpu.load_compressed(&texture).unwrap();
        assert_eq!(WAITS.get(), 0, "BC upload added a completion wait");
        assert_eq!(DRAWS.get(), 0, "BC upload added a residency draw");
        assert_eq!(image.resident_bytes(), 8 * MIB);
        assert_eq!(gpu.pixel(&canvas, 31, 31, false).unwrap(), 0xff193c57);
        assert_eq!(gpu.pixel(&image, 3073, 3073, false).unwrap(), 0xffff0000);
    }
}

#[test]
fn bc3_upload_preserves_all_alpha_and_color_samples_without_waiting() {
    let context = support::Context::new();
    let size = Size {
        width: 16,
        height: 16,
    };
    let budget = Budget::new(MIB);
    let mut bytes = Bytes::zeroed(Format::Bc3Rgba.byte_len(size).unwrap(), &budget).unwrap();
    for (i, block) in bytes
        .as_mut_slice()
        .as_chunks_mut::<16>()
        .0
        .iter_mut()
        .enumerate()
    {
        let (a, b) = if i % 2 == 0 { (23, 231) } else { (227, 17) };
        block[0] = a;
        block[1] = b;
        let alpha_indices = (0..16).fold(0u64, |n, p| n | ((p % 8) << (p * 3)));
        block[2..8].copy_from_slice(&alpha_indices.to_le_bytes()[..6]);
        block[8..12].copy_from_slice(&[0x42, 0xef, 0x7d, 0x18]);
        block[12..].copy_from_slice(&0xe4e4e4e4u32.to_le_bytes());
    }
    let texture = Compressed::new(size, Format::Bc3Rgba, bytes, 0).unwrap();
    // Compare against ordinary deferred upload on this same GPU. DXT endpoint
    // interpolation rounding differs between ANGLE/D3D and the CPU decoder.
    let expected = {
        let baseline = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
        assert!(baseline.supports_compressed(&texture));
        let image = baseline.load_compressed(&texture).unwrap();
        baseline
            .readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .to_vec()
    };
    let gpu = setup(&context);
    let image = gpu.load_compressed(&texture).unwrap();
    assert_eq!(WAITS.get(), 0);
    assert_eq!(DRAWS.get(), 0);
    let actual = gpu.readback(&image, size.rect(), false).unwrap();
    assert_eq!(actual.data.as_slice(), expected);
}
