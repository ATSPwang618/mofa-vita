use crate::{Error, Result, device::Device};
use glow::HasContext;
use std::{cell::Cell, rc::Rc};
mod uniforms;
use uniforms::Uniforms;

struct Uniform {
    location: Option<glow::NativeUniformLocation>,
    value: Cell<Option<(usize, [u32; 4])>>,
}

pub(crate) struct Program {
    device: Rc<Device>,
    pub name: glow::NativeProgram,
    uniforms: Uniforms<Uniform>,
}
impl Program {
    /// The linker removes samplers unused by a specialized draw variant.
    /// Binding them still costs driver validation and can resolve pending work.
    pub fn uses(&self, name: &'static str) -> bool {
        self.uniforms[name].location.is_some()
    }
    #[track_caller]
    pub fn new(device: Rc<Device>, vertex: &str, fragment: &str) -> Result<Self> {
        let caller = std::panic::Location::caller();
        let gl = &device.gl;
        unsafe {
            let program = gl.create_program().map_err(Error::Backend)?;
            let mut shaders = Vec::new();
            let result = (|| {
                for (kind, source) in [
                    (glow::VERTEX_SHADER, vertex.to_owned()),
                    (
                        glow::FRAGMENT_SHADER,
                        crate::draw_source::fragment_source(fragment),
                    ),
                ] {
                    let shared_vertex =
                        kind == glow::VERTEX_SHADER && source == include_str!("quad.vert");
                    if shared_vertex && let Some(shader) = device.quad_vertex.get() {
                        gl.attach_shader(program, shader);
                        shaders.push((shader, false));
                        continue;
                    }
                    let shader = gl.create_shader(kind).map_err(Error::Backend)?;
                    let loaded = match device.shader_binaries.load(gl, shader, kind, &source) {
                        Ok(loaded) => loaded,
                        Err(error) => {
                            gl.delete_shader(shader);
                            return Err(error);
                        }
                    };
                    if !loaded {
                        gl.shader_source(shader, &source);
                        gl.compile_shader(shader);
                    }
                    let compiled = gl.get_shader_compile_status(shader);
                    if !compiled {
                        let log = gl.get_shader_info_log(shader);
                        gl.delete_shader(shader);
                        return Err(Error::Backend(format!(
                            "GLES shader compilation {}:{} stage={kind:#x}: {}",
                            caller.file(),
                            caller.line(),
                            log
                        )));
                    }
                    gl.attach_shader(program, shader);
                    if shared_vertex {
                        device.quad_vertex.set(Some(shader));
                    }
                    shaders.push((shader, !shared_vertex));
                }
                gl.bind_attrib_location(program, 0, "a_unit");
                gl.bind_attrib_location(program, 1, "a_uv");
                gl.link_program(program);
                if !gl.get_program_link_status(program) {
                    return Err(Error::Backend(format!(
                        "GLES program link {}:{}: {}",
                        caller.file(),
                        caller.line(),
                        gl.get_program_info_log(program)
                    )));
                }
                Ok(())
            })();
            for (shader, owned) in shaders {
                gl.detach_shader(program, shader);
                if owned {
                    gl.delete_shader(shader);
                }
            }
            if let Err(error) = result {
                gl.delete_program(program);
                return Err(error);
            }
            let uniforms = Uniforms::new(|name| Uniform {
                location: gl.get_uniform_location(program, name),
                value: Cell::new(None),
            });
            device.draw_state.invalidate_program();
            gl.use_program(Some(program));
            for (name, unit) in crate::scene_batch_source::SAMPLERS
                .into_iter()
                .zip(crate::scene_batch_source::UNITS)
            {
                gl.uniform_1_i32(uniforms[name].location.as_ref(), unit as i32);
            }
            for (unit, name) in [(0, "u_source"), (1, "u_backdrop"), (2, "u_lookup")] {
                gl.uniform_1_i32(uniforms[name].location.as_ref(), unit);
            }
            gl.uniform_1_i32(uniforms["u_source2"].location.as_ref(), 3);
            gl.uniform_1_i32(uniforms["u_rule"].location.as_ref(), 3);
            gl.uniform_1_i32(uniforms["u_curve"].location.as_ref(), 4);
            for (unit, name) in [
                (1, "u_right"),
                (2, "u_down"),
                (3, "u_diagonal"),
                (4, "u_mask"),
                (5, "u_previous"),
            ] {
                gl.uniform_1_i32(uniforms[name].location.as_ref(), unit);
            }
            if let Err(error) = device.check() {
                gl.delete_program(program);
                return Err(error);
            }
            Ok(Self {
                device,
                name: program,
                uniforms,
            })
        }
    }
    pub fn bind(&self) {
        self.device
            .draw_state
            .bind(&self.device.gl, self.name, self.device.quad);
    }
    pub fn bind_program(&self) {
        self.device
            .draw_state
            .bind_program(&self.device.gl, self.name);
    }
    pub fn one(&self, name: &'static str, x: f32) {
        if let Some(location) = self.changed(name, [x]) {
            unsafe { self.device.gl.uniform_1_f32(Some(location), x) };
        }
    }
    pub fn two(&self, name: &'static str, x: f32, y: f32) {
        if let Some(location) = self.changed(name, [x, y]) {
            unsafe { self.device.gl.uniform_2_f32(Some(location), x, y) };
        }
    }
    pub fn three(&self, name: &'static str, v: [f32; 3]) {
        if let Some(location) = self.changed(name, v) {
            unsafe {
                self.device
                    .gl
                    .uniform_3_f32(Some(location), v[0], v[1], v[2])
            };
        }
    }
    pub fn four(&self, name: &'static str, v: [f32; 4]) {
        if let Some(location) = self.changed(name, v) {
            unsafe {
                self.device
                    .gl
                    .uniform_4_f32(Some(location), v[0], v[1], v[2], v[3])
            };
        }
    }
    fn changed<const N: usize>(
        &self,
        name: &'static str,
        values: [f32; N],
    ) -> Option<&glow::NativeUniformLocation> {
        let uniform = &self.uniforms[name];
        let location = uniform.location.as_ref()?;
        let mut bits = [0; 4];
        for (slot, value) in bits.iter_mut().zip(values) {
            *slot = value.to_bits();
        }
        let value = Some((N, bits));
        if uniform.value.get() == value {
            return None;
        }
        // Uniform storage belongs to the linked program and survives binds of
        // other programs, including the work-surface copy program. Every float
        // uniform write to this program goes through these setters.
        uniform.value.set(value);
        Some(location)
    }
}
impl Drop for Program {
    fn drop(&mut self) {
        self.device.retire_program(self.name);
    }
}
