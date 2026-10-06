use super::{Device, Texture};
use crate::Result;
use glow::HasContext;
use krkr_protocol::budget::Budget;

struct Entry {
    texture: glow::NativeTexture,
    framebuffer: glow::NativeFramebuffer,
    bytes: usize,
    scratch: bool,
}

/// Deleting a GL FBO does not guarantee that PVR releases its texture-owned
/// surface. Keep the engine's slot charged until the texture is destroyed.
pub(super) struct Targets {
    entries: Vec<Entry>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    scratch: Budget,
}
impl Targets {
    pub fn new(max_entries: usize, max_bytes: usize, scratch: Budget) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
            max_bytes,
            bytes: 0,
            scratch,
        }
    }
    pub fn select(
        &mut self,
        device: &Device,
        texture: &Texture,
        streaming: bool,
    ) -> Result<Option<glow::NativeFramebuffer>> {
        if let Some(name) = self.get(texture.name()) {
            return Ok(Some(name));
        }
        let bytes = texture.allocation_bytes();
        let scratch = texture.belongs_to(&self.scratch);
        // Long-lived canvases must not consume every native target. Reserve
        // half the bounded pool for composition and filter intermediates.
        let reserved = self.max_entries / 2;
        let split = reserved != 0;
        let entries = self.entries.iter().filter(|entry| entry.scratch == scratch);
        let (used_entries, used_bytes) = entries.fold((0, 0), |(count, bytes), entry| {
            (count + 1, bytes + entry.bytes)
        });
        let entry_limit = if !split {
            self.max_entries
        } else if scratch {
            reserved
        } else {
            self.max_entries - reserved
        };
        let byte_limit = if split {
            self.max_bytes / 2
        } else {
            self.max_bytes
        };
        // Avoid native render surfaces for constants and tiny one-off glyphs.
        // Two writes indicate a canvas reused beyond its initial contents.
        if self.entries.len() >= self.max_entries
            || used_entries >= entry_limit
            || bytes < 16 * 1024
            || bytes > self.max_bytes.saturating_sub(self.bytes)
            || bytes > byte_limit.saturating_sub(used_bytes)
            || (!streaming && !scratch && texture.generation.get() < 2)
        {
            return Ok(None);
        }
        let name = device.create_framebuffer(texture)?;
        self.entries.push(Entry {
            texture: texture.name(),
            framebuffer: name,
            bytes,
            scratch,
        });
        self.bytes += bytes;
        Ok(Some(name))
    }
    pub fn get(&self, texture: glow::NativeTexture) -> Option<glow::NativeFramebuffer> {
        self.entries
            .iter()
            .find(|entry| entry.texture == texture)
            .map(|entry| entry.framebuffer)
    }
    fn evict(&mut self, gl: &glow::Context, index: usize) {
        let entry = self.entries.remove(index);
        self.bytes -= entry.bytes;
        unsafe { gl.delete_framebuffer(entry.framebuffer) };
    }
    pub fn remove(&mut self, gl: &glow::Context, texture: glow::NativeTexture) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.texture == texture)
        {
            self.evict(gl, index);
        }
    }
    pub fn clear(&mut self, gl: &glow::Context) {
        while !self.entries.is_empty() {
            self.evict(gl, self.entries.len() - 1);
        }
    }
}
