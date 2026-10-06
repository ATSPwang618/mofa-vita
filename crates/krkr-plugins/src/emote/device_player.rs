//! D3D's script adapter shares the Motion player, including clone ownership.
//! Rendering uses the portable mesh protocol and the device's layer target.
use super::{manager, player};
use crate::draw_device::{device, layer};
use extensions::GpuImage;
use krkr_engine::{
    extensions,
    plugins::{Context, Exports},
    protocol::{budget::Budget, graphics::Size, mesh::Batch},
};
use std::sync::atomic::Ordering;
use tjs_bind::{IntoTjs, RestArgs, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value, value};

#[tjs_bind::class(name = "D3DEmotePlayer")]
pub(crate) mod bindings {
    use super::*;
    #[derive(Clone, tjs_bind::Trace)]
    pub struct State {
        pub layer: Value,
        pub player: Value,
        pub shown: bool,
        pub animating: bool,
        pub smoothing: bool,
        pub mesh: f64,
        pub bust: f64,
        pub hair: f64,
        pub parts: f64,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                layer: Value::Void,
                player: Value::Void,
                shown: false,
                animating: false,
                smoothing: true,
                mesh: 1.,
                bust: 1.,
                hair: 1.,
                parts: 1.,
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<Self> {
            let class = manager::bindings::install(cx.heap_mut())?;
            let manager = Value::Obj(
                cx.heap_mut()
                    .alloc_native(class, manager::bindings::State::default())?
                    .into(),
            );
            let class = player::bindings::install(cx.heap_mut())?;
            let player = Value::Obj(
                cx.heap_mut()
                    .alloc_native(
                        class,
                        player::bindings::State {
                            manager,
                            ..Default::default()
                        },
                    )?
                    .into(),
            );
            Ok(Self {
                layer,
                player,
                ..Default::default()
            })
        }
        #[tjs::method(resumable = true)]
        fn load(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let (layer, player) = cx.with_state::<Self, _>(|s, _| Ok((s.layer, s.player)))?;
            if super::target(cx, layer).is_none() {
                return Ok(NativeStep::Return(Value::Void));
            }
            let manager =
                player::bindings::with_state(cx, crate::exports::object(player)?, |s| s.manager)?;
            Load {
                owner: cx.this(),
                player,
                manager,
                paths: args.to_vec(),
                index: 0,
            }
            .next(cx)
        }
        #[tjs::method]
        fn show(&mut self) {
            self.shown = true;
        }
        #[tjs::method(name = "clone", resumable = true)]
        fn clone_player(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
            let mut state = cx.with_state::<Self, _>(|s, _| Ok(s.clone()))?;
            state.layer = layer;
            let player = state.player;
            let class = install(cx.heap_mut())?;
            let object = Value::Obj(cx.heap_mut().alloc_native(class, state)?.into());
            // The reference clone retains the very same native player and calls
            // play on it; keep that observable shared playback behavior.
            let empty = String::new().into_tjs(cx.heap_mut())?;
            let step = super::call(cx, player, "play", vec![empty, Value::Int(0)])?;
            Ok(flow::then(
                step,
                flow::callback(object, |object, _, _| Ok(NativeStep::Return(object))),
            ))
        }
        #[tjs::method(resumable = true)]
        fn progress(cx: &mut NativeCx<'_>, frames: f64) -> NativeResult<NativeStep> {
            let owner = cx.this();
            let player = cx.with_state::<Self, _>(|s, _| Ok(s.player))?;
            let step = super::call(
                cx,
                player,
                "progress",
                vec![Value::Real(frames * 1000. / 60.)],
            )?;
            Ok(flow::then(
                step,
                flow::callback(owner, |owner, cx, _| super::draw(cx, owner)),
            ))
        }
        #[tjs::method(resumable = true)]
        fn contains(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() < 2 {
                return Err(NativeError::Missing(args.len()));
            }
            let labelled = args.len() >= 3 && !matches!(args[0], Value::Real(_));
            let index = usize::from(labelled);
            let (layer, player) = cx.with_state::<Self, _>(|s, _| Ok((s.layer, s.player)))?;
            let x = value::to_real(cx.heap(), args[index])? as f32;
            let y = value::to_real(cx.heap(), args[index + 1])? as f32;
            let [x, y] = super::target(cx, layer).map_or([x, y], |(_, _, size, m)| {
                [
                    m[0] * x + m[4] * y + m[12] + size.width as f32 * 0.5,
                    m[1] * x + m[5] * y + m[13] + size.height as f32 * 0.5,
                ]
            });
            let mut args = if labelled { vec![args[0]] } else { Vec::new() };
            args.extend([Value::Real(x as f64), Value::Real(y as f64)]);
            super::call(cx, player, "contains", args)
        }
        #[tjs::method]
        fn finalize(&self) {} // Reference finalizer does not delete the instance.
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.player = Value::Void;
            self.layer = Value::Void;
        }
        #[tjs::getter(name = "animating")]
        fn animating(&self) -> bool {
            self.animating
        }
        #[tjs::setter(name = "animating")]
        fn set_animating(&mut self, v: bool) {
            self.animating = v;
        }
        #[tjs::getter(name = "smoothing")]
        fn smoothing(&self) -> bool {
            self.smoothing
        }
        #[tjs::setter(name = "smoothing")]
        fn set_smoothing(&mut self, v: bool) {
            self.smoothing = v;
        }
        #[tjs::getter(name = "meshDivisionRatio")]
        fn mesh(&self) -> f64 {
            self.mesh
        }
        #[tjs::setter(name = "meshDivisionRatio")]
        fn set_mesh(&mut self, v: f64) {
            self.mesh = v;
        }
        #[tjs::getter(name = "bustScale")]
        fn bust(&self) -> f64 {
            self.bust
        }
        #[tjs::setter(name = "bustScale")]
        fn set_bust(&mut self, v: f64) {
            self.bust = v;
        }
        #[tjs::getter(name = "hairScale")]
        fn hair(&self) -> f64 {
            self.hair
        }
        #[tjs::setter(name = "hairScale")]
        fn set_hair(&mut self, v: f64) {
            self.hair = v;
        }
        #[tjs::getter(name = "partsScale")]
        fn parts(&self) -> f64 {
            self.parts
        }
        #[tjs::setter(name = "partsScale")]
        fn set_parts(&mut self, v: f64) {
            self.parts = v;
        }
    }
}
fn call(
    cx: &mut NativeCx<'_>,
    player: Value,
    name: &str,
    arguments: Vec<Value>,
) -> NativeResult<NativeStep> {
    Ok(NativeStep::CallMember {
        object: player,
        key: name.to_owned().into_tjs(cx.heap_mut())?,
        arguments,
        continuation: flow::callback((), |_, _, value| Ok(NativeStep::Return(value))),
    })
}
#[derive(tjs_bind::Trace)]
struct Load {
    owner: ObjId,
    player: Value,
    manager: Value,
    paths: Vec<Value>,
    index: usize,
}
impl Load {
    fn next(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if let Some(&path) = self.paths.get(self.index) {
            self.index += 1;
            let step = call(cx, self.manager, "load", vec![path])?;
            return Ok(flow::then(
                step,
                flow::callback(self, |s, cx, _| s.next(cx)),
            ));
        }
        let empty = String::new().into_tjs(cx.heap_mut())?;
        let step = call(cx, self.player, "play", vec![empty, Value::Int(0)])?;
        Ok(flow::then(
            step,
            flow::callback(self.owner, |owner, cx, _| {
                bindings::with_state(cx, owner, |s| s.animating = true)?;
                Ok(NativeStep::Return(Value::Void))
            }),
        ))
    }
}

#[derive(Clone, tjs_bind::Trace)]
struct Forward {
    method: &'static str,
    required: usize,
    arity: usize,
    rotation: bool,
}
#[tjs_bind::function(resumable = true)]
fn forward(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    let function = cx.function().ok_or(NativeError::This)?;
    let spec = cx
        .heap_mut()
        .with_native_state::<Forward, _>(function, |s| s.clone())?;
    if args.len() < spec.required {
        return Err(NativeError::Missing(args.len()));
    }
    let mut arguments = args.iter().take(spec.arity).copied().collect::<Vec<_>>();
    arguments.resize(spec.arity, Value::Int(0));
    if spec.method == "stopTimeline" && args.is_empty() {
        arguments[0] = String::new().into_tjs(cx.heap_mut())?;
    }
    if spec.rotation {
        arguments[0] = Value::Real(value::to_real(cx.heap(), arguments[0])?.to_degrees());
    }
    let player = cx.with_state::<bindings::State, _>(|s, _| Ok(s.player))?;
    call(cx, player, spec.method, arguments)
}

#[derive(Clone, tjs_bind::Trace)]
struct TimelineQuery {
    kind: i8,
    index: bool,
    numeric: bool,
}
#[tjs_bind::function]
fn timelines(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Value> {
    let function = cx.function().ok_or(NativeError::This)?;
    let spec = cx
        .heap_mut()
        .with_native_state::<TimelineQuery, _>(function, |s| s.clone())?;
    let index = if spec.index {
        Some(value::to_integer(cx.heap(), crate::exports::arg(args, 0)?)?)
    } else {
        None
    };
    let player = cx.with_state::<bindings::State, _>(|s, _| Ok(s.player))?;
    let labels = player::bindings::with_state(cx, crate::exports::object(player)?, |s| {
        s.playback
            .main
            .as_ref()
            .and_then(|name| s.playback.files.get(name))
            .map_or_else(Vec::new, |state| {
                if spec.kind == 2 {
                    state
                        .timelines
                        .iter()
                        .map(|&i| state.file.metadata.timelines[i].label.clone())
                        .collect()
                } else {
                    state
                        .file
                        .metadata
                        .timelines
                        .iter()
                        .filter(|t| t.diff == spec.kind)
                        .map(|t| t.label.clone())
                        .collect()
                }
            })
    })?;
    if spec.numeric {
        return Ok(Value::Int(0));
    } // Reference timeline info has no flags field.
    if let Some(index) = index {
        labels
            .get(index as usize)
            .cloned()
            .unwrap_or_default()
            .into_tjs(cx.heap_mut())
    } else {
        Ok(Value::Int(labels.len() as i64))
    }
}
#[tjs_bind::function]
fn ratio(_name: tjs_bind::Utf16) -> f64 {
    0.
} // Reference info dictionaries omit blendRatio.

pub(crate) fn install(cx: &mut Context<'_>) -> NativeResult<ObjId> {
    // These methods belong to the registered class, not the global plugin name.
    // Existing instances retain them after plugin exports are unlinked.
    let mut exports = Exports::default();
    let class = bindings::install(cx.heap)?;
    for (name, method, required, arity) in [
        ("skip", "skip", 0, 0),
        ("pass", "pass", 0, 0),
        ("setCoord", "setCoord", 2, 2),
        ("setRot", "setRotate", 1, 1),
        ("setScale", "setScale", 1, 1),
        ("setColor", "setColor", 1, 1),
        ("setVariable", "setVariable", 2, 2),
        ("getVariable", "getVariable", 1, 1),
        ("startWind", "startWind", 5, 5),
        ("stopWind", "stopWind", 0, 0),
        ("playTimeline", "playTimeline", 1, 2),
        ("stopTimeline", "stopTimeline", 0, 1),
        ("fadeOutTimeline", "fadeOutTimeline", 1, 3),
        ("setTimelineBlendRatio", "setTimelineBlendRatio", 2, 4),
        ("isTimelinePlaying", "getTimelinePlaying", 1, 1),
    ] {
        exports.captured_function(
            cx,
            class,
            name,
            forward::CALL,
            Forward {
                method,
                required,
                arity,
                rotation: name == "setRot",
            },
            false,
        )?;
    }
    for (name, kind, index, numeric) in [
        ("countMainTimelines", 0, false, false),
        ("getMainTimelineLabelAt", 0, true, false),
        ("countDiffTimelines", 1, false, false),
        ("getDiffTimelineLabelAt", 1, true, false),
        ("countPlayingTimelines", 2, false, false),
        ("getPlayingTimelineLabelAt", 2, true, false),
        ("getPlayingTimelineFlagsAt", 2, true, true),
    ] {
        exports.captured_function(
            cx,
            class,
            name,
            timelines::CALL,
            TimelineQuery {
                kind,
                index,
                numeric,
            },
            false,
        )?;
    }
    exports.function(cx, class, "getTimelineBlendRatio", ratio::CALL)?;
    Ok(class)
}

fn target(cx: &mut NativeCx<'_>, layer: Value) -> Option<(ObjId, ObjId, Size, [f32; 16])> {
    let id = crate::exports::object(layer).ok()?;
    let (device, matrix) = layer::bindings::with_state(cx, id, |s| (s.device, s.matrix)).ok()?;
    let device = crate::exports::object(device).ok()?;
    let (width, height) = device::bindings::with_state(cx, device, |s| (s.width, s.height)).ok()?;
    if width <= 0 || height <= 0 {
        return None;
    }
    Some((
        id,
        device,
        Size {
            width: width as u32,
            height: height as u32,
        },
        matrix,
    ))
}
fn draw(cx: &mut NativeCx<'_>, owner: ObjId) -> NativeResult<NativeStep> {
    let (layer, player, shown) = bindings::with_state(cx, owner, |s| (s.layer, s.player, s.shown))?;
    if !shown {
        return Ok(NativeStep::Return(Value::Void));
    }
    let Some((layer, device, size, matrix)) = target(cx, layer) else {
        return Ok(NativeStep::Return(Value::Void));
    };
    let player = crate::exports::object(player)?;
    let state = player::bindings::with_state(cx, player, |s| {
        s.playback.selected.as_ref()?;
        let sx = if matrix[0].abs() < 0.01 {
            1.
        } else {
            matrix[0].abs()
        };
        let sy = if matrix[5].abs() < 0.01 {
            1.
        } else {
            matrix[5].abs()
        };
        s.transform.size = [
            (size.width as f32 / sx) as i32 as f32,
            (size.height as f32 / sy) as i32 as f32,
        ];
        s.transform.origin = [
            ((size.width as f32 * 0.5 + matrix[12]) / sx) as i32 as f32,
            ((size.height as f32 * 0.5 + matrix[13]) / sy) as i32 as f32,
        ];
        s.transform.viewport = Some([size.width as f32, size.height as f32]);
        s.transform.depth = s
            .playback
            .files
            .values()
            .fold(30f32, |v, f| v.max(f.file.z_max * 2.));
        s.transform.update();
        s.drawing.generation = s.drawing.generation.wrapping_add(1);
        Some((
            s.drawing.snapshot.clone(),
            std::mem::take(&mut s.drawing.textures),
            s.drawing.generation,
            s.playback.playing,
        ))
    })?;
    let Some((snapshot, mut textures, generation, animating)) = state else {
        return Ok(NativeStep::Return(Value::Void));
    };
    bindings::with_state(cx, owner, |s| s.animating = animating)?;
    let layer_generation = layer::bindings::with_state(cx, layer, |s| {
        s.emote_generation = s.emote_generation.wrapping_add(1);
        s.emote_generation
    })?;
    let budget = extensions::image_staging_budget(cx.heap_mut())?
        .unwrap_or_else(|| Budget::new(64 * 1024 * 1024));
    extensions::run_work(
        cx,
        move |stop| {
            let result = (|| {
                let frame = if let Some(snapshot) = snapshot {
                    let (playback, transform) = snapshot.as_ref();
                    super::scene::build(playback, transform, &mut textures, &budget, &|| {
                        stop.load(Ordering::Relaxed)
                    })?
                } else {
                    super::render::Frame::default()
                };
                frame.into_batch(true)
            })();
            Ok(Rendered { result, textures })
        },
        Box::new(Written {
            owner,
            player,
            layer,
            device,
            generation,
            layer_generation,
            size,
        }),
    )
}
struct Rendered {
    result: NativeResult<(Batch, Vec<super::mesh::HitArea>)>,
    textures: super::render::Textures,
}
#[derive(tjs_bind::Trace)]
struct Written {
    owner: ObjId,
    player: ObjId,
    layer: ObjId,
    device: ObjId,
    generation: u64,
    layer_generation: u64,
    #[trace(skip = "Pixel dimensions contain no VM handles")]
    size: Size,
}
impl extensions::WorkContinuation<Rendered> for Written {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        rendered: Rendered,
    ) -> NativeResult<NativeStep> {
        let current = bindings::with_state(
            cx,
            self.owner,
            |s| matches!(s.player, Value::Obj(r) if r.object == Some(self.player)),
        )?;
        let current = current
            && player::bindings::with_state(cx, self.player, |s| {
                if s.drawing.generation != self.generation {
                    return false;
                }
                s.drawing.textures = rendered.textures;
                true
            })?;
        if !current {
            return Err(NativeError::Message(
                "E-mote player changed while rendering",
            ));
        }
        let (batch, shapes) = rendered.result?;
        let previous = layer::bindings::with_state(cx, self.layer, |s| s.emote.clone())?;
        let target = match previous.filter(|p| p.size == self.size) {
            Some(image) => image,
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
        if !player::bindings::with_state(cx, self.player, |s| {
            s.drawing.generation == self.generation
        })? {
            return Err(NativeError::Message(
                "E-mote player changed while rendering",
            ));
        }
        let device_current = device::bindings::with_state(cx, self.device, |s| {
            s.width == self.size.width as i32 && s.height == self.size.height as i32
        })?;
        let current = device_current
            && layer::bindings::with_state(cx, self.layer, |s| {
                if s.emote_generation != self.layer_generation
                    || !matches!(s.device, Value::Obj(r) if r.object == Some(self.device))
                {
                    return false;
                }
                s.emote = Some(pixels);
                true
            })?;
        if !current {
            return Err(NativeError::Message(
                "E-mote device target changed while rendering",
            ));
        }
        player::bindings::with_state(cx, self.player, |s| s.drawing.shapes = shapes)?;
        Ok(NativeStep::Return(Value::Void))
    }
}
