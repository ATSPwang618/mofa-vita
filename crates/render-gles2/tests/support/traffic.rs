//! Observe real GLES calls in this test executable. No renderer test hooks or
//! counters are linked into the library. Unit 7 belongs to the work-surface blit.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
};
type Active = unsafe extern "system" fn(u32);
type Draw = unsafe extern "system" fn(u32, i32, i32);
type Viewport = unsafe extern "system" fn(i32, i32, i32, i32);
type Copy = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, i32, i32);
type Clear = unsafe extern "system" fn(u32);
type TexImage = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
type ReadPixels = unsafe extern "system" fn(i32, i32, i32, i32, u32, u32, *mut c_void);
type BufferData = unsafe extern "system" fn(u32, isize, *const c_void, u32);
type Finish = unsafe extern "system" fn();
type UseProgram = unsafe extern "system" fn(u32);
type UniformLocation = unsafe extern "system" fn(u32, *const c_char) -> i32;
type Uniform4 = unsafe extern "system" fn(i32, f32, f32, f32, f32);
type GetInteger = unsafe extern "system" fn(u32, *mut i32);
thread_local! {
    static GET_INTEGER_FN: Cell<Option<GetInteger>> = const { Cell::new(None) };
    static USE_PROGRAM_FN: Cell<Option<UseProgram>> = const { Cell::new(None) };
    static LOCATION_FN: Cell<Option<UniformLocation>> = const { Cell::new(None) };
    static UNIFORM4_FN: Cell<Option<Uniform4>> = const { Cell::new(None) };
    static PROGRAM: Cell<u32> = const { Cell::new(0) };
    static DESTINATIONS: RefCell<HashMap<u32, (i32, [f32; 4])>> = RefCell::new(HashMap::new());
    static FINISH_FN: Cell<Option<Finish>> = const { Cell::new(None) };
    static FINISHES: Cell<usize> = const { Cell::new(0) };
    static BUFFER_FN: Cell<Option<BufferData>> = const { Cell::new(None) };
    static BUFFER_UPLOADS: Cell<usize> = const { Cell::new(0) };
    static ACTIVE_FN: Cell<Option<Active>> = const { Cell::new(None) };
    static DRAW_FN: Cell<Option<Draw>> = const { Cell::new(None) };
    static VIEWPORT_FN: Cell<Option<Viewport>> = const { Cell::new(None) };
    static VIEWPORT_CALLS: Cell<usize> = const { Cell::new(0) };
    static VIEWPORT_CHANGES: Cell<usize> = const { Cell::new(0) };
    static VIEWPORT_VALUE: Cell<Option<[i32; 4]>> = const { Cell::new(None) };
    static COPY_FN: Cell<Option<Copy>> = const { Cell::new(None) };
    static CLEAR_FN: Cell<Option<Clear>> = const { Cell::new(None) };
    static TEX_IMAGE_FN: Cell<Option<TexImage>> = const { Cell::new(None) };
    static TEXTURE_ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static READ_FN: Cell<Option<ReadPixels>> = const { Cell::new(None) };
    static READS: Cell<usize> = const { Cell::new(0) };
    static DRAWS: Cell<usize> = const { Cell::new(0) };
    static CLEARS: Cell<usize> = const { Cell::new(0) };
    static UNIT: Cell<u32> = const { Cell::new(glow::TEXTURE0) };
    static PIXELS: Cell<usize> = const { Cell::new(0) };
    static EXTENT: Cell<[i32; 2]> = const { Cell::new([0; 2]) };
    static LOADS: Cell<usize> = const { Cell::new(0) };
    static LOAD_CALLS: Cell<usize> = const { Cell::new(0) };
    static STORES: Cell<usize> = const { Cell::new(0) };
    static STORE_CALLS: Cell<usize> = const { Cell::new(0) };
}
pub fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glGetIntegerv" => {
            GET_INTEGER_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, GetInteger>(address)
            }));
            get_integer as *const c_void
        }
        "glUseProgram" => {
            USE_PROGRAM_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, UseProgram>(address)
            }));
            PROGRAM.set(0);
            use_program as *const c_void
        }
        "glGetUniformLocation" => {
            LOCATION_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, UniformLocation>(address)
            }));
            DESTINATIONS.with_borrow_mut(HashMap::clear);
            uniform_location as *const c_void
        }
        "glUniform4f" => {
            UNIFORM4_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Uniform4>(address)
            }));
            uniform4 as *const c_void
        }
        "glFinish" => {
            assert!(!address.is_null());
            FINISH_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Finish>(address)
            }));
            finish as *const c_void
        }
        "glBufferData" => {
            assert!(!address.is_null());
            BUFFER_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, BufferData>(address)
            }));
            buffer_data as *const c_void
        }
        "glTexImage2D" => {
            assert!(!address.is_null());
            TEX_IMAGE_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, TexImage>(address)
            }));
            tex_image as *const c_void
        }
        "glReadPixels" => {
            READ_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, ReadPixels>(address)
            }));
            read_pixels as *const c_void
        }
        "glClear" => {
            assert!(!address.is_null());
            CLEAR_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Clear>(address)
            }));
            clear as *const c_void
        }
        "glActiveTexture" => {
            assert!(!address.is_null());
            ACTIVE_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Active>(address)
            }));
            UNIT.set(glow::TEXTURE0);
            active as *const c_void
        }
        "glDrawArrays" => {
            assert!(!address.is_null());
            DRAW_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Draw>(address)
            }));
            draw as *const c_void
        }
        "glViewport" => {
            assert!(!address.is_null());
            VIEWPORT_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Viewport>(address)
            }));
            VIEWPORT_VALUE.set(None);
            viewport as *const c_void
        }
        "glCopyTexSubImage2D" => {
            assert!(!address.is_null());
            COPY_FN.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Copy>(address)
            }));
            copy as *const c_void
        }
        _ => address,
    }
}
unsafe extern "system" fn finish() {
    let _profile = krkr_protocol::profile::span("gl.finish");
    FINISHES.set(FINISHES.get() + 1);
    unsafe { FINISH_FN.get().unwrap()() };
}
unsafe extern "system" fn buffer_data(target: u32, size: isize, data: *const c_void, usage: u32) {
    BUFFER_UPLOADS.set(BUFFER_UPLOADS.get() + 1);
    unsafe { BUFFER_FN.get().unwrap()(target, size, data, usage) };
}
unsafe extern "system" fn tex_image(
    target: u32,
    level: i32,
    internal: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    kind: u32,
    pixels: *const c_void,
) {
    TEXTURE_ALLOCATIONS.set(TEXTURE_ALLOCATIONS.get() + 1);
    let _profile = krkr_protocol::profile::span_detail("gl.tex_image", || {
        format!("size={width}x{height} format={format:#x}")
    });
    unsafe {
        TEX_IMAGE_FN.get().unwrap()(
            target, level, internal, width, height, border, format, kind, pixels,
        )
    };
}
unsafe extern "system" fn read_pixels(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    format: u32,
    kind: u32,
    pixels: *mut c_void,
) {
    READS.set(READS.get() + 1);
    let _profile =
        krkr_protocol::profile::span_detail("gl.read_pixels", || format!("size={width}x{height}"));
    unsafe { READ_FN.get().unwrap()(x, y, width, height, format, kind, pixels) };
}
unsafe extern "system" fn active(unit: u32) {
    UNIT.set(unit);
    unsafe { ACTIVE_FN.get().unwrap()(unit) };
}
unsafe extern "system" fn draw(mode: u32, first: i32, count: i32) {
    DRAWS.set(DRAWS.get() + 1);
    if UNIT.get() == glow::TEXTURE7 {
        // Work loads may use a sub-rectangle quad without shrinking viewport.
        let pixels = DESTINATIONS.with_borrow(|destinations| {
            destinations
                .get(&PROGRAM.get())
                .map_or(PIXELS.get(), |(_, placement)| {
                    let extent = EXTENT.get();
                    (extent[0].max(0) as f64 * f64::from(placement[2]))
                        .round()
                        .max(0.) as usize
                        * (extent[1].max(0) as f64 * f64::from(placement[3]))
                            .round()
                            .max(0.) as usize
                })
        });
        LOADS.set(LOADS.get() + pixels);
        LOAD_CALLS.set(LOAD_CALLS.get() + 1);
    }
    unsafe { DRAW_FN.get().unwrap()(mode, first, count) };
}
unsafe extern "system" fn clear(mask: u32) {
    CLEARS.set(CLEARS.get() + 1);
    unsafe { CLEAR_FN.get().unwrap()(mask) };
}
unsafe extern "system" fn viewport(x: i32, y: i32, width: i32, height: i32) {
    VIEWPORT_CALLS.set(VIEWPORT_CALLS.get() + 1);
    if VIEWPORT_VALUE.replace(Some([x, y, width, height])) != Some([x, y, width, height]) {
        VIEWPORT_CHANGES.set(VIEWPORT_CHANGES.get() + 1);
    }
    PIXELS.set(width.max(0) as usize * height.max(0) as usize);
    EXTENT.set([width, height]);
    unsafe { VIEWPORT_FN.get().unwrap()(x, y, width, height) };
}
unsafe extern "system" fn use_program(program: u32) {
    PROGRAM.set(program);
    unsafe { USE_PROGRAM_FN.get().unwrap()(program) };
}
unsafe extern "system" fn get_integer(parameter: u32, values: *mut i32) {
    unsafe { GET_INTEGER_FN.get().unwrap()(parameter, values) };
    if parameter == glow::VIEWPORT {
        let values = unsafe { std::slice::from_raw_parts(values, 4) };
        VIEWPORT_VALUE.set(Some([values[0], values[1], values[2], values[3]]));
        EXTENT.set([values[2], values[3]]);
        PIXELS.set(values[2].max(0) as usize * values[3].max(0) as usize);
    }
}
unsafe extern "system" fn uniform_location(program: u32, name: *const c_char) -> i32 {
    let location = unsafe { LOCATION_FN.get().unwrap()(program, name) };
    if unsafe { CStr::from_ptr(name) }.to_bytes() == b"u_destination" && location >= 0 {
        DESTINATIONS.with_borrow_mut(|values| {
            values.insert(program, (location, [0.; 4]));
        });
    }
    location
}
unsafe extern "system" fn uniform4(location: i32, x: f32, y: f32, z: f32, w: f32) {
    DESTINATIONS.with_borrow_mut(|values| {
        if let Some((expected, placement)) = values.get_mut(&PROGRAM.get())
            && *expected == location
        {
            *placement = [x, y, z, w];
        }
    });
    unsafe { UNIFORM4_FN.get().unwrap()(location, x, y, z, w) };
}
unsafe extern "system" fn copy(
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) {
    STORES.set(STORES.get() + width.max(0) as usize * height.max(0) as usize);
    let _profile = krkr_protocol::profile::span_detail("gl.copy_tex_sub_image", || {
        format!("size={width}x{height}")
    });
    STORE_CALLS.set(STORE_CALLS.get() + 1);
    unsafe { COPY_FN.get().unwrap()(target, level, xoffset, yoffset, x, y, width, height) };
}
pub fn reset() {
    VIEWPORT_CALLS.set(0);
    VIEWPORT_CHANGES.set(0);
    FINISHES.set(0);
    BUFFER_UPLOADS.set(0);
    assert!(ACTIVE_FN.get().is_some() && DRAW_FN.get().is_some() && VIEWPORT_FN.get().is_some());
    LOADS.set(0);
    LOAD_CALLS.set(0);
    STORES.set(0);
    STORE_CALLS.set(0);
    DRAWS.set(0);
    CLEARS.set(0);
    READS.set(0);
    TEXTURE_ALLOCATIONS.set(0);
}
#[allow(dead_code)]
pub fn viewport_calls() -> usize {
    VIEWPORT_CALLS.get()
}
#[allow(dead_code)]
pub fn viewport_changes() -> usize {
    VIEWPORT_CHANGES.get()
}
#[allow(dead_code)]
pub fn finish_calls() -> usize {
    FINISHES.get()
}
#[allow(dead_code)]
pub fn buffer_uploads() -> usize {
    BUFFER_UPLOADS.get()
}
#[allow(dead_code)]
pub fn texture_allocations() -> usize {
    TEXTURE_ALLOCATIONS.get()
}
#[allow(dead_code)]
pub fn read_calls() -> usize {
    READS.get()
}
#[allow(dead_code)]
pub fn loaded_pixels() -> usize {
    LOADS.get()
}
#[allow(dead_code)]
pub fn load_calls() -> usize {
    LOAD_CALLS.get()
}
#[allow(dead_code)] // Other integration tests use only the load counter.
pub fn stored_pixels() -> usize {
    STORES.get()
}
#[allow(dead_code)]
pub fn store_calls() -> usize {
    STORE_CALLS.get()
}
#[allow(dead_code)]
pub fn draw_calls() -> usize {
    DRAWS.get()
}
#[allow(dead_code)]
pub fn clear_calls() -> usize {
    CLEARS.get()
}
