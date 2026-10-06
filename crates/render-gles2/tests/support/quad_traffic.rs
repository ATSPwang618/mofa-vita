//! Count submitted quad areas from real GL uniforms, outside the renderer.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
};
type Location = unsafe extern "system" fn(u32, *const c_char) -> i32;
type Use = unsafe extern "system" fn(u32);
type Uniform = unsafe extern "system" fn(i32, f32, f32, f32, f32);
type Draw = unsafe extern "system" fn(u32, i32, i32);
thread_local! {
    static LOCATION: Cell<Option<Location>> = const { Cell::new(None) };
    static USE: Cell<Option<Use>> = const { Cell::new(None) };
    static UNIFORM: Cell<Option<Uniform>> = const { Cell::new(None) };
    static DRAW: Cell<Option<Draw>> = const { Cell::new(None) };
    static CURRENT: Cell<u32> = const { Cell::new(0) };
    static RECTS: RefCell<HashMap<u32, (i32, usize)>> = RefCell::default();
    static AREA: Cell<usize> = const { Cell::new(0) };
}
pub fn intercept(name: &str, address: *const c_void) -> *const c_void {
    match name {
        "glGetUniformLocation" => {
            LOCATION.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Location>(address)
            }));
            location as *const c_void
        }
        "glUseProgram" => {
            RECTS.with_borrow_mut(HashMap::clear);
            CURRENT.set(0);
            USE.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Use>(address)
            }));
            use_program as *const c_void
        }
        "glUniform4f" => {
            UNIFORM.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Uniform>(address)
            }));
            uniform as *const c_void
        }
        "glDrawArrays" => {
            DRAW.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Draw>(address)
            }));
            draw as *const c_void
        }
        _ => address,
    }
}
unsafe extern "system" fn location(program: u32, name: *const c_char) -> i32 {
    let value = unsafe { LOCATION.get().unwrap()(program, name) };
    if unsafe { CStr::from_ptr(name) }.to_bytes() == b"u_rectangle" {
        RECTS.with_borrow_mut(|r| {
            r.insert(program, (value, 0));
        });
    }
    value
}
unsafe extern "system" fn use_program(program: u32) {
    CURRENT.set(program);
    unsafe { USE.get().unwrap()(program) };
}
unsafe extern "system" fn uniform(location: i32, x: f32, y: f32, w: f32, h: f32) {
    RECTS.with_borrow_mut(|r| {
        if let Some((id, area)) = r.get_mut(&CURRENT.get())
            && *id == location
        {
            *area = (w.max(0.) * h.max(0.)) as usize;
        }
    });
    unsafe { UNIFORM.get().unwrap()(location, x, y, w, h) };
}
unsafe extern "system" fn draw(mode: u32, first: i32, count: i32) {
    if mode == glow::TRIANGLE_STRIP && count == 4 {
        RECTS.with_borrow(|r| {
            if let Some((_, area)) = r.get(&CURRENT.get()) {
                AREA.set(AREA.get() + area);
            }
        });
    }
    unsafe { DRAW.get().unwrap()(mode, first, count) };
}
pub fn reset() {
    AREA.set(0);
}
pub fn pixels() -> usize {
    AREA.get()
}
