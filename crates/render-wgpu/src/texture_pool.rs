//! Bounded reuse of small resident allocations after all image versions and
//! submitted readers have released them. Cached storage keeps its budget permit.
use crate::gpu::Allocation;
use krkr_protocol::graphics::Size;
use std::{collections::VecDeque, sync::Arc};

#[derive(Default)]
pub(crate) struct TexturePool {
    entries: VecDeque<Arc<Allocation>>,
    bytes: usize,
}
impl TexturePool {
    const BYTES: usize = 16 * 1024 * 1024;
    const SLOTS: usize = 256;

    fn bytes(image: &Allocation) -> usize {
        image.texture.width() as usize
            * image.texture.height() as usize
            * if image.texture.format() == wgpu::TextureFormat::R8Unorm {
                1
            } else {
                4
            }
    }
    pub fn find(&mut self, size: Size, format: wgpu::TextureFormat) -> Option<Arc<Allocation>> {
        let index = self.entries.iter().position(|image| {
            Arc::strong_count(image) == 1
                && image.texture.width() == size.width
                && image.texture.height() == size.height
                && image.texture.format() == format
        })?;
        let image = self.entries.remove(index).expect("pooled texture");
        self.entries.push_back(image.clone());
        Some(image)
    }
    pub fn remember(&mut self, image: &Arc<Allocation>, budget: usize) {
        let limit = Self::BYTES.min(budget / 16);
        let bytes = Self::bytes(image);
        // Large canvases have a different lifetime and should leave capacity
        // for scene composition. This pool targets repeatedly rebuilt controls.
        if bytes > (1024 * 1024).min(limit) {
            return;
        }
        while self.entries.len() >= Self::SLOTS || self.bytes + bytes > limit {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= Self::bytes(&old);
        }
        self.bytes += bytes;
        self.entries.push_back(image.clone());
    }
    pub fn trim(&mut self) {
        self.entries.retain(|image| Arc::strong_count(image) > 1);
        self.bytes = self.entries.iter().map(|image| Self::bytes(image)).sum();
    }
}
