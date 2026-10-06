//! Publish an already completed backend image through the ordinary window scene.
use super::*;
use crate::window::draw_device;
use tjs_core::{NativeCx, NativeStep};
pub(crate) struct Frame {
    pub image: ImageRef,
    pub size: Size,
}
impl Layers {
    pub(crate) fn clear_device_frame(&mut self, window: WindowId) {
        self.device_frames.remove(&window);
        self.dirty.insert(window);
    }
}
pub fn present(
    cx: &mut NativeCx<'_>,
    window: Value,
    device: ObjId,
    image: super::gpu_image::GpuImage,
) -> NativeResult<NativeStep> {
    draw_device::begin_frame(cx, object_id(window)?, device)?;
    let (shared, window_id) = draw_device::world(cx, window)?;
    let mut world = shared.borrow_mut();
    world.clear_device_frame(window_id);
    world.device_frames.insert(
        window_id,
        Frame {
            image: image.image(),
            size: image.size,
        },
    );
    world.windows.borrow_mut().recheck_viewport(window_id);
    Ok(NativeStep::Return(Value::Void))
}
