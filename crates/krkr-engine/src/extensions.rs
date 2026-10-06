//! Narrow access to engine state for portable, host-registered plugin providers.
pub use crate::layer::PixelContinuation;
pub use crate::layer::gpu_image::{
    GpuImage, ImageContinuation, allocate as gpu_image, copy_region as layer_copy_image_region,
    draw as draw_meshes, patch as layer_patch_image, snapshot as layer_snapshot_image,
    upload as upload_image,
};
pub use crate::layer::snapshot::capture as layer_snapshot_subtree;
mod font;
mod text;
pub use font::{FontFace, list_font_families, resolve_font_source};
pub(crate) mod worker;
pub use crate::sound::visualization::{SampleBuffer, sample_buffer, validate as validate_sound};
pub use text::{FontMetricsContinuation, ImageSizeContinuation, image_size, measure_characters};
pub use worker::{WorkContinuation, run_work};
pub fn register_audio_decoder(
    heap: &mut Heap,
    kind: crate::audio::Codec,
) -> NativeResult<crate::audio::Registration> {
    crate::sound::register_decoder(heap, kind)
}
pub fn layer_read_planes(
    cx: &mut NativeCx<'_>,
    layer: Value,
    next: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    layer_size(cx, layer)?;
    crate::layer::bitmap::read_planes(cx, layer, next, true)
}
pub fn layer_patch_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: std::sync::Arc<crate::protocol::pixels::Pixels>,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::patch_pixels(cx, layer, pixels)
}
/// Read a contained main-plane rectangle in image coordinates.
pub fn layer_read_region(
    cx: &mut NativeCx<'_>,
    layer: Value,
    rectangle: Rect,
    next: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    crate::layer::bitmap::read_region(cx, layer, rectangle, next)
}
pub fn layer_patch_region(
    cx: &mut NativeCx<'_>,
    layer: Value,
    rectangle: Rect,
    pixels: std::sync::Arc<crate::protocol::pixels::Pixels>,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::patch_region(cx, layer, rectangle, pixels)
}
/// Complete script painting and copy the entire composed Layer subtree.
pub fn layer_capture_subtree(
    cx: &mut NativeCx<'_>,
    destination: Value,
    source: Value,
) -> NativeResult<NativeStep> {
    crate::layer::piled::capture(cx, destination, source)
}
/// Read the composed subtree, including pending onPaint callbacks and children.
pub fn layer_read_subtree(
    cx: &mut NativeCx<'_>,
    source: Value,
    next: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    crate::layer::snapshot::read(cx, source, next)
}
/// Register a VFS font privately for subsequent enumeration and rendering.
pub fn register_font(cx: &mut NativeCx<'_>, filename: &[u16]) -> NativeResult<NativeStep> {
    crate::font::tasks::register(cx, filename)
}
pub fn layer_read_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    next: Box<dyn PixelContinuation>,
) -> NativeResult<NativeStep> {
    layer_size(cx, layer)?;
    crate::layer::read_pixels(cx, layer, next)
}
pub use crate::system::extension::encode_image;
/// The same monotonic clock used by System.getTickCount and animation timers.
pub use crate::system::extension::tick_count;
pub use crate::system::extension::{Continuous, continuous};
pub use crate::video::extension::{
    Handle as VideoHandle, Opened as VideoOpened, open as open_effect_video, rewind as rewind_video,
};

pub fn layer_copy_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: std::sync::Arc<crate::protocol::pixels::Pixels>,
    split_alpha: bool,
    size: Size,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::copy_pixels(cx, layer, pixels, split_alpha, size)
}
pub fn layer_copy_yuv(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: std::sync::Arc<crate::protocol::pixels::Yuv420>,
    logical_size: Size,
    split_alpha: bool,
    size: Size,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::copy_yuv(cx, layer, pixels, logical_size, split_alpha, size)
}
pub use crate::layer::transition::provider::{
    Definition as TransitionDefinition, Registration as TransitionRegistration,
    register as register_transitions,
};
use crate::protocol::graphics::{Adjustment, Rect, Size};
use tjs_core::{Heap, NativeCx, NativeResult, NativeStep, ObjId, Value};

pub use crate::system::extension::{breathe, clear_graphic_cache, is_breathing, log_eval_error};
pub use crate::window::desktop::{request as desktop_request, window_id};
pub use crate::window::extension::set_client_rect;
pub use crate::window::icons::{reset_window_icon, set_application_icon, set_window_icon};

pub use crate::window::extension::{
    Property as WindowProperty, Rectangle as WindowRectangle, control as window_control,
    get as window_get, rectangle as window_rectangle, set as window_set, z_order as window_z_order,
};

pub fn register_window_events(heap: &mut Heap, window: ObjId, has_move: bool) -> NativeResult<()> {
    crate::window::bindings::register_extended_events(heap, window, has_move)
}

pub use crate::layer::device::present as present_draw_device;
pub use crate::window::draw_device::detach as detach_draw_device;
pub use crate::window::draw_device::input_manager as draw_device_input_manager;
pub use crate::window::draw_device::{
    register as register_draw_device, roots as window_root_layers,
};
pub use crate::window::screen::{Target as WindowScreenTarget, target as window_screen_target};

pub fn layer_size(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<Size> {
    crate::layer::extension_size(cx, layer)
}
/// Actual image dimensions, independent of the layer's display rectangle.
pub fn layer_image_size(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<Size> {
    crate::layer::extensions::image_size(cx, layer)
}
/// Resize the main image and display rectangle in one ordered graphics operation.
pub fn layer_resize_image_and_layer(
    cx: &mut NativeCx<'_>,
    layer: Value,
    width: i32,
    height: i32,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::resize_image_and_layer(cx, layer, width, height)
}
/// Managed equivalent of acquiring the plugin's writable main image. Actual
/// pixel allocation/detachment remains in the ordered graphics operation.
pub fn layer_prepare_draw(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<()> {
    crate::layer::extension_prepare_draw(cx, layer)
}
pub fn layer_adjust(
    cx: &mut NativeCx<'_>,
    layer: Value,
    rectangle: Rect,
    operation: Adjustment,
    clip: bool,
) -> NativeResult<NativeStep> {
    crate::layer::extension_adjust(cx, layer, rectangle, operation, clip)
}
pub fn layer_wrapped_copy(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    clip: Rect,
) -> NativeResult<NativeStep> {
    crate::layer::extension_wrapped_copy(cx, args, clip)
}

pub fn layer_copy_scanlines(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: Value,
    rows: std::sync::Arc<crate::protocol::scanlines::Scanlines>,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::scanlines(cx, layer, source, rows)
}
pub fn layer_warp(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: Value,
    effect: std::sync::Arc<crate::protocol::warp::Warp>,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::warp(cx, layer, source, effect)
}
pub fn layer_draw_sprites(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: Value,
    batch: std::sync::Arc<crate::protocol::sprites::Sprites>,
    mode: i32,
) -> NativeResult<NativeStep> {
    crate::layer::extensions::sprites(cx, layer, source, batch, mode)
}
pub fn layer_clear_main(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
    crate::layer::extensions::clear_main(cx, layer)
}

pub fn layer_perspective(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    crate::layer::extension_perspective(cx, args)
}

pub fn layer_pixel_budget(
    cx: &mut NativeCx<'_>,
    layer: Value,
) -> NativeResult<crate::protocol::budget::Budget> {
    crate::layer::extension_size(cx, layer)?;
    crate::layer::pixel_budget(cx, layer)
}
pub fn layer_write_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: crate::protocol::pixels::Pixels,
) -> NativeResult<NativeStep> {
    crate::layer::extension_size(cx, layer)?;
    crate::layer::write_pixels(cx, layer, pixels)
}

pub fn image_staging_budget(
    heap: &mut Heap,
) -> NativeResult<Option<crate::protocol::budget::Budget>> {
    crate::window::clipboard::staging_budget(heap)
}

/// Assign an owned shared main image, retaining its allocation permit through upload.
pub fn layer_write_shared_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: std::sync::Arc<crate::protocol::pixels::Pixels>,
    display_size: Size,
) -> NativeResult<NativeStep> {
    layer_size(cx, layer)?;
    crate::layer::write_shared_pixels(cx, layer, pixels, display_size)
}
