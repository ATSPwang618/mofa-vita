//! Script update callbacks run on the VM. GPU drawing consumes an immutable
//! snapshot afterward, so callbacks can edit or delete pictures safely.
use super::{device::bindings, image, layer, picture};
use krkr_engine::{
    extensions::{self, GpuImage},
    protocol::{
        budget::Budget,
        graphics::Size,
        mesh::{Batch, Blend, Draw, Geometry, Texture, Vertex},
    },
};
use tjs_bind::{IntoTjs, flow};
use tjs_core::{
    NativeCx, NativeError, NativeResult, NativeStep, NativeTryContinuation, ObjId, Trace, Value,
};

pub(super) fn start(cx: &mut NativeCx<'_>, diff: f64) -> NativeResult<NativeStep> {
    let owner = cx.this();
    super::device::refresh_roots(cx, owner)?;
    let (layers, generation) = bindings::with_state(cx, owner, |s| {
        if s.transition_active {
            s.transition_progress = (1. - s.trans_state).clamp(0., 1.);
        }
        s.generation = s.generation.wrapping_add(1);
        (s.layers.clone(), s.generation)
    })?;
    Update {
        owner,
        layers,
        next: 0,
        diff,
        generation,
    }
    .advance(cx)
}
#[derive(tjs_bind::Trace)]
struct Update {
    owner: ObjId,
    layers: Vec<Value>,
    next: usize,
    diff: f64,
    generation: u64,
}
impl Update {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if !bindings::with_state(cx, self.owner, |s| s.generation == self.generation)? {
            return Err(NativeError::Message("draw device changed during update"));
        }
        if let Some(&layer) = self.layers.get(self.next) {
            self.next += 1;
            let diff = self.diff;
            return Ok(NativeStep::Try {
                task: flow::callback((layer, diff), |(layer, diff), cx, _| {
                    Ok(NativeStep::CallMemberOr {
                        object: layer,
                        key: "onUpdate".to_owned().into_tjs(cx.heap_mut())?,
                        arguments: vec![Value::Real(diff)],
                        result_needed: false,
                        continuation: flow::callback((), |_, _, v| Ok(NativeStep::Return(v))),
                    })
                }),
                continuation: Box::new(self),
            });
        }
        render(cx, self.owner, self.generation)
    }
}
impl NativeTryContinuation for Update {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        _: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        // DriveOnUpdate catches script errors in the reference. Cancellation
        // still propagates through the VM instead of becoming a successful call.
        self.advance(cx)
    }
}
enum Item {
    Picture(Picture),
    Emote(GpuImage),
}
struct Picture {
    image: GpuImage,
    matrix: [f32; 16],
    source: [i32; 4],
    position: [f32; 2],
    blend: i32,
    opacity: i32,
}
fn render(cx: &mut NativeCx<'_>, owner: ObjId, generation: u64) -> NativeResult<NativeStep> {
    let (width, height, offset, layers, page, previous, progress, active) =
        bindings::with_state(cx, owner, |s| {
            (
                s.width,
                s.height,
                s.offset,
                s.layers.clone(),
                if s.transition_active {
                    3 - s.page
                } else {
                    s.page
                },
                s.previous.clone(),
                s.transition_progress,
                s.transition_active,
            )
        })?;
    if width <= 0 || height <= 0 {
        return Err(NativeError::Message("invalid draw device extent"));
    }
    let size = Size {
        width: width as u32,
        height: height as u32,
    };
    let budget = extensions::image_staging_budget(cx.heap_mut())?
        .unwrap_or_else(|| Budget::new(64 * 1024 * 1024));
    let mut sorted = Vec::new();
    for layer in layers {
        let Ok(id) = crate::exports::object(layer) else {
            continue;
        };
        if let Ok((plane, front, matrix, pictures, emote)) =
            layer::bindings::with_state(cx, id, |s| {
                (
                    s.plane,
                    s.front,
                    s.matrix,
                    s.pictures.clone(),
                    s.emote.clone(),
                )
            })
            && (plane != 2 || page == 2)
        {
            sorted.push(((plane == 2, front), matrix, pictures, emote));
        }
    }
    sorted.sort_by_key(|v| v.0);
    let mut items = Vec::new();
    for (_, matrix, pictures, emote) in sorted {
        for picture in pictures {
            let Ok(id) = crate::exports::object(picture) else {
                continue;
            };
            let Ok((image, source, destination, coordinate, blend, opacity)) =
                picture::bindings::with_state(cx, id, |s| {
                    (
                        s.image,
                        s.source,
                        s.destination,
                        s.coordinate,
                        s.blend,
                        s.opacity,
                    )
                })
            else {
                continue;
            };
            let Ok(id) = crate::exports::object(image) else {
                continue;
            };
            let Ok(Some(image)) = image::bindings::with_state(cx, id, |s| s.image.clone()) else {
                continue;
            };
            let position =
                std::array::from_fn(|i| destination[i] as f32 + coordinate[i] + offset[i] as f32);
            items.push(Item::Picture(Picture {
                image,
                matrix,
                source,
                position,
                blend,
                opacity,
            }));
        }
        if let Some(emote) = emote {
            items.push(Item::Emote(emote));
        }
    }
    let sources = bindings::with_state(cx, owner, |s| {
        let mut sources = Vec::new();
        if let Some(&layer) = s.primary.get(page as usize) {
            sources.push(layer);
        }
        if s.manager_index == 3
            && let Some(&layer) = s.primary.get(3)
        {
            sources.push(layer);
        }
        sources
    })?;
    bindings::with_state(cx, owner, |s| {
        s.manager_images.retain(|(layer, _)| {
            sources
                .iter()
                .any(|&source| crate::exports::object(source).ok() == Some(*layer))
        })
    })?;
    Composite {
        owner,
        generation,
        size,
        budget,
        items,
        previous,
        progress,
        active,
        sources,
        planes: Vec::new(),
    }
    .next(cx)
}
struct Composite {
    owner: ObjId,
    generation: u64,
    size: Size,
    budget: Budget,
    items: Vec<Item>,
    previous: Option<GpuImage>,
    progress: f64,
    active: bool,
    sources: Vec<Value>,
    planes: Vec<GpuImage>,
}
impl Trace for Composite {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.sources.trace(visit);
    }
}
impl Composite {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if !bindings::with_state(cx, self.owner, |s| s.generation == self.generation)? {
            return Err(NativeError::Message(
                "draw device changed during Layer composition",
            ));
        }
        if let Some(&source) = self.sources.get(self.planes.len()) {
            let previous = bindings::with_state(cx, self.owner, |s| {
                s.manager_images
                    .iter()
                    .find(|(layer, _)| Some(*layer) == crate::exports::object(source).ok())
                    .map(|(_, image)| image.clone())
            })?;
            return extensions::layer_snapshot_subtree(cx, source, previous, Box::new(self));
        }
        self.render(cx)
    }
    fn render(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let mut draws = Vec::new();
        for item in self.items {
            match item {
                Item::Emote(image) => {
                    draws.push(fullscreen(&self.budget, image, Blend::LayerAlpha, 1.)?)
                }
                Item::Picture(item) => {
                    let [sx, sy, sw, sh] = item.source;
                    if sw <= 0
                        || sh <= 0
                        || item.image.size.width == 0
                        || item.image.size.height == 0
                    {
                        continue;
                    }
                    let [x, y] = item.position;
                    let positions = [
                        [x, y],
                        [x + sw as f32, y],
                        [x + sw as f32, y + sh as f32],
                        [x, y + sh as f32],
                    ];
                    let u0 = sx as f32 / item.image.size.width as f32;
                    let v0 = sy as f32 / item.image.size.height as f32;
                    let u1 = (sx as f64 + sw as f64) as f32 / item.image.size.width as f32;
                    let v1 = (sy as f64 + sh as f64) as f32 / item.image.size.height as f32;
                    let uv = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
                    let m = item.matrix;
                    let vertices = positions
                        .into_iter()
                        .enumerate()
                        .map(|(i, [x, y])| Vertex {
                            position: [
                                (m[0] * x + m[4] * y + m[12]) / self.size.width as f32 * 2.,
                                (m[1] * x + m[5] * y + m[13]) / self.size.height as f32 * 2.,
                            ],
                            uv: uv[i],
                        })
                        .collect();
                    draws.push(quad(
                        &self.budget,
                        item.image,
                        vertices,
                        if item.blend == 5 {
                            Blend::MultiplyAdd
                        } else {
                            Blend::AlphaMax
                        },
                        item.opacity as f32 / 255.,
                    )?);
                }
            }
        }
        for image in self.planes {
            draws.push(fullscreen(&self.budget, image, Blend::LayerAlpha, 1.)?);
        }
        if self.active
            && self.progress < 1.
            && let Some(image) = self.previous
        {
            draws.push(fullscreen(
                &self.budget,
                image,
                Blend::AlphaMax,
                (1. - self.progress) as f32,
            )?);
        }
        let previous = bindings::with_state(cx, self.owner, |s| {
            s.composite
                .as_ref()
                .filter(|p| p.size == self.size && p.is_unique())
                .cloned()
        })?;
        let target = match previous {
            Some(image) => image,
            None => extensions::gpu_image(cx, self.size)?,
        };
        let batch = Batch {
            order: (0..draws.len()).collect(),
            draws,
            clear: Some([0.; 4]),
        };
        extensions::draw_meshes(
            cx,
            target,
            batch,
            Box::new(Written {
                owner: self.owner,
                generation: self.generation,
                width: self.size.width as i32,
                height: self.size.height as i32,
                finish_transition: self.active && self.progress >= 1.,
            }),
        )
    }
}
impl extensions::ImageContinuation for Composite {
    fn image(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        image: GpuImage,
    ) -> NativeResult<NativeStep> {
        bindings::with_state(cx, self.owner, |s| {
            let source = crate::exports::object(self.sources[self.planes.len()])
                .expect("validated source Layer");
            s.manager_images.retain(|(layer, _)| *layer != source);
            s.manager_images.push((source, image.clone()));
        })?;
        self.planes.push(image);
        self.next(cx)
    }
}
fn quad(
    budget: &Budget,
    image: GpuImage,
    vertices: Vec<Vertex>,
    blend: Blend,
    opacity: f32,
) -> NativeResult<Draw> {
    let permit = budget
        .reserve(vertices.capacity() * std::mem::size_of::<Vertex>() + 6 * 2)
        .map_err(|e| NativeError::Detail(e.to_string()))?;
    Ok(Draw {
        geometry: Geometry {
            vertices,
            indices: vec![0, 1, 2, 2, 3, 0],
            _permit: permit,
        },
        texture: Texture::Image(image.image()),
        blend,
        opacity,
        color: [1.; 4],
        solid_color: false,
        masks: vec![],
        visible: true,
    })
}
fn fullscreen(budget: &Budget, image: GpuImage, blend: Blend, opacity: f32) -> NativeResult<Draw> {
    let vertices = [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]]
        .into_iter()
        .map(|position| Vertex {
            position,
            uv: position.map(|v| (v + 1.) * 0.5),
        })
        .collect();
    quad(budget, image, vertices, blend, opacity)
}
#[derive(tjs_bind::Trace)]
struct Written {
    owner: ObjId,
    generation: u64,
    width: i32,
    height: i32,
    finish_transition: bool,
}
impl extensions::ImageContinuation for Written {
    fn image(self: Box<Self>, cx: &mut NativeCx<'_>, pixels: GpuImage) -> NativeResult<NativeStep> {
        let valid = bindings::with_state(cx, self.owner, |s| {
            if s.generation != self.generation || s.width != self.width || s.height != self.height {
                return false;
            }
            s.composite = Some(pixels.clone());
            if self.finish_transition {
                s.stop_transition();
            }
            true
        })?;
        if !valid {
            return Err(NativeError::Message("draw device changed while composing"));
        }
        let window = bindings::with_state(cx, self.owner, |s| s.window)?;
        if matches!(window, Value::Void) {
            return Ok(NativeStep::Return(Value::Void));
        }
        extensions::present_draw_device(cx, window, self.owner, pixels)
    }
}
