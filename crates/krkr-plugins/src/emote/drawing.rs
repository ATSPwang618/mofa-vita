//! Layer output evaluates mesh geometry on the shared worker
//! executor. The continuation checks player and target lifetime before writing.
use super::{playback::Playback, player::bindings, render::Textures, scene, transform::Transform};
use extensions::GpuImage;
use krkr_engine::{
    extensions,
    protocol::{graphics::Size, mesh::Batch},
};
use std::sync::{Arc, atomic::Ordering};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};

#[derive(Default)]
pub(super) struct Drawing {
    // Draws and queries share the immutable state captured by progress().
    pub snapshot: Option<Arc<(Playback, Transform)>>,
    pub textures: Textures,
    pub target: Option<GpuImage>,
    pub generation: u64,
    pub shapes: Vec<super::mesh::HitArea>,
    pub ping_pong: bool,
}
pub(super) fn start(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let separate = crate::exports::object(layer).ok().and_then(|id| {
        super::adaptor::bindings::with_state(cx, id, |s| {
            s.generation = s.generation.wrapping_add(1);
            (id, s.generation, s.layer, s.target.clone())
        })
        .ok()
    });
    let layer = separate.as_ref().map_or(layer, |s| s.2);
    if separate.is_some() && matches!(layer, Value::Void) {
        return Ok(NativeStep::Return(Value::Void));
    }
    let offscreen = crate::exports::object(layer).ok().and_then(|id| {
        super::offscreen::bindings::with_state(cx, id, |s| {
            s.generation = s.generation.wrapping_add(1);
            (
                id,
                s.generation,
                s.size,
                s.origin,
                s.image.clone(),
                s.budget.clone(),
            )
        })
        .ok()
    });
    let (size, budget) = if let Some((_, _, size, _, _, budget)) = &offscreen {
        (*size, budget.clone())
    } else {
        (
            extensions::layer_size(cx, layer)?,
            extensions::layer_pixel_budget(cx, layer)?,
        )
    };
    let (snapshot, mut textures, previous, generation, clear) =
        bindings::with_state(cx, owner, |s| {
            let depth = s
                .playback
                .files
                .values()
                .fold(30f32, |v, f| v.max(f.file.z_max * 2.));
            s.transform
                .resize([size.width as f32, size.height as f32], depth);
            if let Some((_, _, _, origin, _, _)) = &offscreen {
                s.transform.depth = depth;
                s.transform.origin = [
                    origin[0] as f32 - size.width as f32 / 2.,
                    origin[1] as f32 - size.height as f32 / 2.,
                ];
                s.transform.update();
            }
            s.drawing.generation = s.drawing.generation.wrapping_add(1);
            (
                s.drawing.snapshot.clone(),
                std::mem::take(&mut s.drawing.textures),
                offscreen.as_ref().map_or_else(
                    || {
                        separate
                            .as_ref()
                            .map_or_else(|| s.drawing.target.clone(), |v| v.3.clone())
                    },
                    |v| v.4.clone(),
                ),
                s.drawing.generation,
                offscreen.is_none() && s.playback.self_clear,
            )
        })?;
    let Some(snapshot) = snapshot else {
        bindings::with_state(cx, owner, |s| s.drawing.textures = textures)?;
        return Ok(NativeStep::Return(Value::Void));
    };
    extensions::run_work(
        cx,
        move |stop| {
            let (playback, transform) = snapshot.as_ref();
            let result = (|| {
                let cancelled = || stop.load(Ordering::Relaxed);
                let frame = scene::build(playback, transform, &mut textures, &budget, &cancelled)?;
                frame.into_batch(clear)
            })();
            // Return the cache even on render errors, preserving its byte permits.
            Ok(Completed { textures, result })
        },
        Box::new(Written {
            owner,
            layer,
            size,
            generation,
            previous,
            offscreen: offscreen.map(|v| (v.0, v.1)),
            separate: separate.map(|v| (v.0, v.1)),
        }),
    )
}
struct Completed {
    textures: Textures,
    result: NativeResult<(Batch, Vec<super::mesh::HitArea>)>,
}
#[derive(tjs_bind::Trace)]
struct Written {
    owner: ObjId,
    layer: Value,
    #[trace(skip = "Plain pixel dimensions")]
    size: Size,
    generation: u64,
    previous: Option<GpuImage>,
    offscreen: Option<(ObjId, u64)>,
    separate: Option<(ObjId, u64)>,
}
impl extensions::WorkContinuation<Completed> for Written {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        completed: Completed,
    ) -> NativeResult<NativeStep> {
        let valid = bindings::with_state(cx, self.owner, |s| {
            if s.drawing.generation != self.generation {
                return false;
            }
            s.drawing.textures = completed.textures;
            true
        })?;
        if !valid {
            return Err(NativeError::Message("E-mote player changed while drawing"));
        }
        let (batch, shapes) = completed.result?;
        let target = match self.previous.as_ref().filter(|p| p.size == self.size) {
            Some(target) => target.clone(),
            None => extensions::gpu_image(cx, self.size)?,
        };
        extensions::draw_meshes(
            cx,
            target,
            batch,
            Box::new(Published {
                written: *self,
                shapes,
            }),
        )
    }
}
#[derive(tjs_bind::Trace)]
struct Published {
    written: Written,
    #[trace(skip = "Owned hit geometry")]
    shapes: Vec<super::mesh::HitArea>,
}
impl extensions::ImageContinuation for Published {
    fn image(self: Box<Self>, cx: &mut NativeCx<'_>, pixels: GpuImage) -> NativeResult<NativeStep> {
        let Self { written, shapes } = *self;
        written.publish(cx, pixels, shapes)
    }
}
impl Written {
    fn publish(
        self,
        cx: &mut NativeCx<'_>,
        pixels: GpuImage,
        shapes: Vec<super::mesh::HitArea>,
    ) -> NativeResult<NativeStep> {
        if !bindings::with_state(cx, self.owner, |s| s.drawing.generation == self.generation)? {
            return Err(NativeError::Message("E-mote player changed while drawing"));
        }
        if self.offscreen.is_none() && extensions::layer_size(cx, self.layer)? != self.size {
            return Err(NativeError::Message("E-mote target resized while drawing"));
        }
        if let Some((id, generation)) = self.offscreen {
            let valid = super::offscreen::bindings::with_state(cx, id, |s| {
                if s.generation != generation {
                    return false;
                }
                s.image = Some(pixels.clone());
                true
            })?;
            if !valid {
                return Err(NativeError::Message("E-mote canvas changed while drawing"));
            }
            bindings::with_state(cx, self.owner, |s| s.drawing.shapes = shapes)?;
            return Ok(NativeStep::Return(Value::Void));
        }
        bindings::with_state(cx, self.owner, |s| {
            if self.separate.is_none() {
                s.drawing.target = Some(pixels.clone());
            }
            s.drawing.shapes = shapes;
        })?;
        if let Some((id, generation)) = self.separate {
            let valid = super::adaptor::bindings::with_state(cx, id, |s| {
                if s.generation != generation {
                    return false;
                }
                s.target = Some(pixels.clone());
                true
            })?;
            if !valid {
                return Err(NativeError::Message(
                    "E-mote layer adaptor changed while drawing",
                ));
            }
        }
        extensions::layer_patch_image(cx, self.layer, pixels)
    }
}
