use glow::HasContext;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    num::NonZeroU32,
};

type Draw = unsafe extern "system" fn(u32, i32, i32);
thread_local! {
    static DRAW: Cell<Option<Draw>> = const { Cell::new(None) };
    static AUDIT: RefCell<Option<Audit>> = const { RefCell::new(None) };
}
struct Audit {
    gl: glow::Context,
    conflicts: Vec<String>,
}
pub struct Check;
impl Check {
    pub fn new(gl: glow::Context) -> Self {
        AUDIT.with_borrow_mut(|audit| {
            assert!(audit.is_none());
            *audit = Some(Audit {
                gl,
                conflicts: Vec::new(),
            });
        });
        Self
    }
    pub fn conflicts(&self) -> Vec<String> {
        AUDIT.with_borrow(|audit| audit.as_ref().unwrap().conflicts.clone())
    }
}
impl Drop for Check {
    fn drop(&mut self) {
        AUDIT.with_borrow_mut(|audit| {
            audit.take();
        });
    }
}
pub fn intercept(name: &str, address: *const c_void) -> *const c_void {
    if name != "glDrawArrays" {
        return address;
    }
    DRAW.set(Some(unsafe {
        std::mem::transmute::<*const c_void, Draw>(address)
    }));
    draw as *const c_void
}
unsafe extern "system" fn draw(mode: u32, first: i32, count: i32) {
    AUDIT.with_borrow_mut(|audit| {
        let Some(audit) = audit else {
            return;
        };
        let gl = &audit.gl;
        unsafe {
            let framebuffer = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            if framebuffer == 0
                || gl.get_framebuffer_attachment_parameter_i32(
                    glow::FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
                ) != glow::TEXTURE as i32
            {
                return;
            }
            let target = gl.get_framebuffer_attachment_parameter_i32(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
            );
            let Some(program) = NonZeroU32::new(gl.get_parameter_i32(glow::CURRENT_PROGRAM) as u32)
                .map(glow::NativeProgram)
            else {
                return;
            };
            let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
            for i in 0..gl.get_active_uniforms(program) {
                let uniform = gl.get_active_uniform(program, i).unwrap();
                if uniform.utype != glow::SAMPLER_2D {
                    continue;
                }
                let location = gl.get_uniform_location(program, &uniform.name).unwrap();
                let mut unit = [0];
                gl.get_uniform_i32(program, &location, &mut unit);
                gl.active_texture(glow::TEXTURE0 + unit[0] as u32);
                if gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) == target {
                    audit.conflicts.push(format!(
                        "framebuffer={framebuffer} texture={target} program={program:?} sampler={}",
                        uniform.name
                    ));
                }
            }
            gl.active_texture(active as u32);
        }
    });
    unsafe { DRAW.get().unwrap()(mode, first, count) };
}
