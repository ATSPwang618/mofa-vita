//! Reuse CPU frame storage only after the queue, engine and renderer release it.
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Yuv420, Yuv420Layout},
};
use std::sync::Arc;

// Four queued frames plus presentation/render references fit without turning
// the entire movie into a cache. Every retained allocation keeps its permit.
const RETAINED_FRAMES: usize = 8;

#[derive(Default)]
pub struct FramePool {
    frames: Vec<Arc<Yuv420>>,
}
impl FramePool {
    pub fn acquire(&mut self, size: Size, budget: &Budget) -> Result<Arc<Yuv420>, String> {
        if let Some(index) = self.frames.iter().position(|frame| {
            frame.size == size && Arc::strong_count(frame) == 1 && Arc::weak_count(frame) == 0
        }) {
            return Ok(self.frames.swap_remove(index));
        }
        let length = Yuv420::byte_len(size).ok_or("invalid visible movie size")?;
        Ok(Arc::new(Yuv420 {
            size,
            layout: Yuv420Layout::Nv12,
            data: Bytes::zeroed(length, budget).map_err(|error| error.to_string())?,
        }))
    }
    pub fn retain(&mut self, frame: &Arc<Yuv420>) {
        // Unexpectedly long-lived script/render references may require another
        // allocation; do not retain those extras after their consumers finish.
        if self.frames.len() < RETAINED_FRAMES {
            self.frames.push(frame.clone());
        }
    }
}
