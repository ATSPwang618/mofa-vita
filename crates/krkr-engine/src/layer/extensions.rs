use super::{
    bindings::{State, integer, rectangle},
    tasks::Change,
    *,
};
use krkr_protocol::graphics::{Adjustment, Command};
use tjs_core::{NativeCx, NativeStep};
pub(crate) fn warp(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: Value,
    effect: Arc<krkr_protocol::warp::Warp>,
) -> NativeResult<NativeStep> {
    let source = cx
        .heap_mut()
        .with_native_state::<State, _>(object_id(source)?, |s| s.image())??;
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.command(
                Command::Warp {
                    image: s.image()?,
                    source,
                    effect,
                },
                Change::None,
            )
        })?
}
pub(crate) fn scanlines(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: Value,
    rows: Arc<krkr_protocol::scanlines::Scanlines>,
) -> NativeResult<NativeStep> {
    let source = cx
        .heap_mut()
        .with_native_state::<State, _>(object_id(source)?, |s| s.image())??;
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.command(
                Command::Scanlines {
                    image: s.image()?,
                    source,
                    rows,
                },
                Change::None,
            )
        })?
}
pub(crate) fn patch_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: Arc<krkr_protocol::pixels::Pixels>,
) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.command(
                Command::PatchPixels {
                    image: s.image()?,
                    pixels,
                },
                Change::None,
            )
        })?
}

pub(crate) fn patch_region(
    cx: &mut NativeCx<'_>,
    layer: Value,
    rectangle: Rect,
    pixels: Arc<krkr_protocol::pixels::Pixels>,
) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.command(
                Command::PatchRegion {
                    image: s.image()?,
                    rectangle,
                    pixels,
                },
                Change::None,
            )
        })?
}

pub(crate) fn copy_pixels(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: Arc<krkr_protocol::pixels::Pixels>,
    split_alpha: bool,
    size: Size,
) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.command(
                Command::CopyPixels {
                    image: s.image()?,
                    pixels,
                    split_alpha,
                    size,
                },
                Change::None,
            )
        })?
}

pub(crate) fn copy_yuv(
    cx: &mut NativeCx<'_>,
    layer: Value,
    pixels: Arc<krkr_protocol::pixels::Yuv420>,
    logical_size: Size,
    split_alpha: bool,
    size: Size,
) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.command(
                Command::CopyYuv {
                    image: s.image()?,
                    pixels,
                    logical_size,
                    split_alpha,
                    size,
                },
                Change::None,
            )
        })?
}

pub(crate) fn perspective(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    cx.with_state::<State, _>(|s, cx| {
        s.lease()?; // Native receiver validation precedes argument validation.
        if args.len() < 13 {
            return Err(NativeError::Missing(12));
        }
        // Only Layer's Object facet is accepted, not Bitmap or closure ObjThis.
        let source_id = if object_id(args[0])? == cx.this() {
            s.lease()?.id
        } else {
            bindings::layer_id(cx.heap_mut(), args[0])?
        };
        let mut p = [0.0; 12];
        for (out, input) in p.iter_mut().zip(&args[1..13]) {
            *out = tjs_core::value::to_real(cx.heap(), *input)?;
        }
        let source = {
            let world = s.lease()?.shared.borrow();
            world.record(source_id)?.image()?.clone()
        };
        let image = s.image()?;
        let size = s.read(|r| r.geometry.image_size)?;
        let destination = [[p[4], p[5]], [p[6], p[7]], [p[8], p[9]], [p[10], p[11]]];
        if !p.iter().all(|v| v.is_finite()) {
            return Err(NativeError::Message(
                "perspective coordinates must be finite",
            ));
        }
        // Preserve the plugin's repeated double-to-int truncations, including
        // its exclusive right/bottom scissor. Layer.clip and face are ignored.
        let (mut left, mut top, mut right, mut bottom) =
            (size.width as i32, size.height as i32, 0, 0);
        for [x, y] in destination {
            if f64::from(left) > x {
                left = x as i32;
            }
            if f64::from(top) > y {
                top = y as i32;
            }
            if f64::from(right) < x {
                right = x as i32;
            }
            if f64::from(bottom) < y {
                bottom = y as i32;
            }
        }
        left = left.max(0);
        top = top.max(0);
        right = right.min(size.width as i32);
        bottom = bottom.min(size.height as i32);
        s.command(
            Command::Perspective {
                image,
                source,
                mapping: krkr_protocol::transform::Perspective {
                    // OperatePerspective adds one to the supplied source endpoints.
                    source: [p[0], p[1], p[0] + p[2] + 1.0, p[1] + p[3] + 1.0],
                    destination,
                },
                clip: Rect {
                    left,
                    top,
                    width: right.saturating_sub(left).max(0) as u32,
                    height: bottom.saturating_sub(top).max(0) as u32,
                },
            },
            Change::None,
        )
    })
}
pub(crate) fn size(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<Size> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| s.read(|r| r.geometry.size()))?
}
pub(crate) fn image_size(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<Size> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| s.read(|r| r.geometry.image_size))?
}
pub(crate) fn sprites(
    cx: &mut NativeCx<'_>,
    layer: Value,
    source: Value,
    batch: Arc<krkr_protocol::sprites::Sprites>,
    mode: i32,
) -> NativeResult<NativeStep> {
    let (source, source_mode) = cx
        .heap_mut()
        .with_native_state::<State, _>(object_id(source)?, |s| {
            Ok::<_, NativeError>((s.image()?, s.read(|r| r.blend)?))
        })??;
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.require_main()?;
            let (face, hold_alpha, clip) = s.read(|r| (r.face(), r.hold_alpha, r.geometry.clip))?;
            let options = krkr_protocol::graphics::BlendOptions {
                mode: if mode == 128 {
                    source_mode
                } else {
                    krkr_protocol::graphics::Blend::from_legacy(mode)
                        .ok_or(NativeError::Message("unknown particle operation mode"))?
                },
                face: face?,
                hold_alpha,
                opacity: 255,
            };
            if !options.accepts_face() {
                return Err(NativeError::Message(
                    "particle operation does not support this draw face",
                ));
            }
            s.command(
                Command::Sprites {
                    image: s.image()?,
                    source,
                    batch,
                    clip,
                    options,
                },
                Change::None,
            )
        })?
}
pub(crate) fn clear_main(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.require_main()?;
            let (rectangle, face, hold_alpha) =
                s.read(|r| (r.geometry.image_size.rect(), r.face(), r.hold_alpha))?;
            s.command(
                Command::Fill {
                    image: s.image()?,
                    fills: vec![krkr_protocol::graphics::Fill {
                        rectangle,
                        color: 0,
                        face: face?,
                        hold_alpha,
                    }],
                },
                Change::None,
            )
        })?
}
pub(crate) fn resize_image_and_layer(
    cx: &mut NativeCx<'_>,
    layer: Value,
    width: i32,
    height: i32,
) -> NativeResult<NativeStep> {
    let size = bindings::size(width, height)?;
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.require_main()?;
            let mut geometry = s.read(|r| r.geometry)?;
            geometry.set_size(size);
            s.geometry(geometry, size)
        })?
}
pub(crate) fn prepare_draw(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<()> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            s.require_main()?;
            let lease = s.lease()?;
            lease
                .shared
                .borrow_mut()
                .record_mut(lease.id)?
                .image_modified = true;
            Ok(())
        })?
}
pub(crate) fn adjust(
    cx: &mut NativeCx<'_>,
    layer: Value,
    rect: Rect,
    operation: Adjustment,
    clip: bool,
) -> NativeResult<NativeStep> {
    cx.heap_mut()
        .with_native_state::<State, _>(object_id(layer)?, |s| {
            let image = s.image()?;
            let clip = s.read(|r| {
                if clip {
                    r.geometry.clip
                } else {
                    r.geometry.image_size.rect()
                }
            })?;
            let Some(rectangle) = rect.intersection(clip) else {
                return Ok(NativeStep::Return(Value::Void));
            };
            s.command(
                Command::Adjust {
                    image,
                    rectangle,
                    operation,
                },
                Change::None,
            )
        })?
}
pub(crate) fn wrapped_copy(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    clip: Rect,
) -> NativeResult<NativeStep> {
    if args.len() < 11 {
        return Err(NativeError::Message(
            "copyWrappedRect requires eleven arguments",
        ));
    }
    cx.with_state::<State, _>(|s, cx| {
        // The plugin's source is a Layer main image, not a Bitmap's buffer ABI.
        let source_id = if object_id(args[4])? == cx.this() {
            s.lease()?.id
        } else {
            bindings::layer_id(cx.heap_mut(), args[4])?
        };
        let source = {
            let world = s.lease()?.shared.borrow();
            world.record(source_id)?.image()?.clone()
        };
        let command = Command::WrappedCopy {
            image: s.image()?,
            source,
            destination: rectangle(cx, args)?,
            rectangle: rectangle(cx, &args[5..])?,
            shift: (integer(cx, args[9])?, integer(cx, args[10])?),
            clip,
        };
        s.command(command, Change::None)
    })
}
