use krkr_protocol::pixels::{Bytes, Pixels};
use krkr_render::overlay::{Counter, SIZE};
use krkr_render_gles2::{Gpu, Image};
use std::time::Instant;

pub(crate) struct Overlay {
    counter: Counter,
    image: Option<Image>,
}
impl Overlay {
    pub fn new() -> Self {
        Self {
            counter: Counter::new(Instant::now()),
            image: None,
        }
    }
    pub fn frame(&mut self) {
        self.counter.frame();
    }
    pub fn refresh(&mut self, gpu: &Gpu, now: Instant) -> Result<bool, String> {
        let Some(fps) = self.counter.sample(now) else {
            return Ok(false);
        };
        let data = krkr_render::overlay::pixels(
            fps,
            free_memory(),
            true,
            gpu.resident.used() + gpu.scratch.used(),
        );
        let permit = gpu.staging.reserve(data.len()).map_err(|e| e.to_string())?;
        let pixels = Pixels {
            size: SIZE,
            main: Some(Bytes::with_permit(data, permit)),
            province: None,
        };
        if self.image.is_none() {
            self.image = Some(
                gpu.reserve_upload(SIZE, true, false)
                    .map_err(|e| e.to_string())?,
            );
        }
        gpu.upload(self.image.as_mut().unwrap(), &pixels)
            .map_err(|e| e.to_string())?;
        Ok(true)
    }
    pub fn draw(&self, gpu: &Gpu) -> Result<(), String> {
        if let Some(image) = &self.image {
            gpu.present_cursor(
                image,
                crate::window::DISPLAY,
                krkr_render::Rect {
                    left: 8,
                    top: 8,
                    ..SIZE.rect()
                },
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
fn free_memory() -> Option<usize> {
    // Kernel counters, not mallinfo (which traverses and locks the free list).
    let mut info = vitasdk_sys::SceKernelFreeMemorySizeInfo {
        size: std::mem::size_of::<vitasdk_sys::SceKernelFreeMemorySizeInfo>() as _,
        size_user: 0,
        size_cdram: 0,
        size_phycont: 0,
    };
    (unsafe { vitasdk_sys::sceKernelGetFreeMemorySize(&mut info) } >= 0)
        .then_some(info.size_user.max(0) as usize)
}
#[cfg(not(target_os = "vita"))]
fn free_memory() -> Option<usize> {
    None
}
