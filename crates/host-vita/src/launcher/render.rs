//! Nivora GLES2 renderer scoped to the launcher's current context.
#![allow(unsafe_code)]
use glow::HasContext;
use nivora_platform::{Frame, TextMeasurer};
use nivora_render_gles2::{Gles2Options, Gles2Renderer, Gles2Target};

pub(super) struct LauncherRenderer<'gl> {
    gl: &'gl glow::Context,
    renderer: Gles2Renderer<'gl>,
}
impl<'gl> LauncherRenderer<'gl> {
    pub fn new(target: &'gl Gles2Target<'gl>, gl: &'gl glow::Context) -> Result<Self, String> {
        let renderer = Gles2Renderer::with_ab_glyph_font_and_options(
            target,
            krkr_render::font::bundled::data().to_vec(),
            Gles2Options {
                glyph_page_size: 512,
                cached_glyph_pages: 2,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { gl, renderer })
    }
    pub fn measurer(&self) -> &impl TextMeasurer {
        &self.renderer
    }
    pub fn present(&mut self, frame: &Frame) -> Result<(), String> {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.color_mask(true, true, true, true);
        }
        self.renderer.render(frame).map_err(|e| e.to_string())
    }
}
