use glow::HasContext;
use std::cell::Cell;

/// Reuse bindings only while the renderer owns the context. A host may change
/// GL state between public calls, so the outermost scope discards all knowledge
/// on both entry and exit, including early returns.
#[derive(Default)]
pub(crate) struct DrawState {
    depth: Cell<usize>,
    program: Cell<Option<glow::NativeProgram>>,
    quad: Cell<bool>,
}

pub(crate) struct Scope<'a>(&'a DrawState);

impl DrawState {
    pub fn scope(&self) -> Scope<'_> {
        if self.depth.get() == 0 {
            self.invalidate_program();
            self.invalidate_vertices();
        }
        self.depth.set(self.depth.get() + 1);
        Scope(self)
    }

    pub fn invalidate_program(&self) {
        self.program.set(None);
    }

    pub fn invalidate_vertices(&self) {
        self.quad.set(false);
    }

    pub fn bind_program(&self, gl: &glow::Context, program: glow::NativeProgram) {
        unsafe {
            if self.depth.get() == 0 || self.program.replace(Some(program)) != Some(program) {
                gl.use_program(Some(program));
            }
        }
    }

    pub fn bind(&self, gl: &glow::Context, program: glow::NativeProgram, quad: glow::NativeBuffer) {
        self.bind_program(gl, program);
        unsafe {
            if self.depth.get() == 0 || !self.quad.replace(true) {
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
                gl.enable_vertex_attrib_array(0);
                gl.disable_vertex_attrib_array(1);
                gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            }
        }
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.0.depth.set(self.0.depth.get() - 1);
        if self.0.depth.get() == 0 {
            self.0.invalidate_program();
            self.0.invalidate_vertices();
        }
    }
}
