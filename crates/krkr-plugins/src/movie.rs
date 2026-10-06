//! Portable layerExMovie adaptation; original plugin by Go Watanabe.
//! See movie/LICENSE.txt. DirectShow pointers and temporary file copies are
//! replaced by owned media jobs, continuous hooks and ordered GPU frame writes.
use krkr_engine::{
    extensions::{self, Continuous, VideoHandle, VideoOpened},
    plugins::{Context, Exports, Plugin},
};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    sync::Arc,
};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Trace,
    Value, value,
};

#[derive(Default, Clone)]
struct Shared(Rc<RefCell<Playback>>);
impl std::ops::Deref for Shared {
    type Target = RefCell<Playback>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Trace for Shared {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.borrow().trace(visit);
    }
}
#[derive(Default, Clone)]
struct Registry(Rc<RefCell<Vec<Weak<RefCell<Playback>>>>>);
impl Trace for Registry {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
} // Weak observations do not root movies.
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Movie {
    exports: Exports,
    registry: Registry,
}
krkr_engine::native_plugin! { impl Movie { names: ["layerExMovie.dll", "layerExMovie.tpm"] } }
impl Plugin for Movie {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        cx.heap.register_state_invalidator::<State>(cleanup::CALL);
        let layer = crate::exports::class(cx, "Layer")?;
        let mut exports = Exports::default();
        for (name, call) in [
            ("openMovie", open::CALL),
            ("startMovie", start::CALL),
            ("stopMovie", stop::CALL),
            ("isPlayingMovie", playing::CALL),
        ] {
            exports.captured_function(cx, layer, name, call, self.registry.clone(), false)?;
        }
        self.exports = exports;
        Ok(())
    }
    fn can_unlink(&self, _: &Context<'_>) -> NativeResult<bool> {
        Ok(!self
            .registry
            .0
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .any(|p| {
                let p = p.borrow();
                p.handle.is_some() || p.busy
            }))
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        if !self.can_unlink(cx)? {
            return Ok(false);
        }
        self.registry
            .0
            .borrow_mut()
            .retain(|r| r.strong_count() != 0);
        self.exports.unlink(cx)
    }
}
#[derive(Default, tjs_bind::Trace)]
struct Playback {
    #[trace(skip = "Owned media handle has no TJS values and closes its worker on drop")]
    handle: Option<VideoHandle>,
    hook: Option<Continuous>,
    generation: u64,
    playing: bool,
    looping: bool,
    alpha: bool,
    busy: bool,
}
impl Playback {
    fn clear(&mut self) {
        self.hook = None;
        if let Some(handle) = self.handle.take() {
            handle.play(false);
        }
        self.playing = false;
        self.busy = false;
        self.generation = self.generation.wrapping_add(1);
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.clear();
    }
}
#[derive(Default, tjs_bind::Trace)]
struct State {
    api: Option<[Value; 9]>,
    tick: Option<Value>,
    playback: Shared,
}
const PROPERTIES: [&str; 6] = [
    "imageLeft",
    "imageTop",
    "imageWidth",
    "imageHeight",
    "update",
    "type",
];
const EVENTS: [&str; 3] = ["onStartMovie", "onStopMovie", "onUpdateMovie"];
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Action {
    Open,
    Start,
    Stop,
    Playing,
}
macro_rules! entry {
    ($name:ident,$action:ident) => {
        #[tjs_bind::function(resumable = true)]
        fn $name(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
            initialize(cx, args, Action::$action)
        }
    };
}
entry!(open, Open);
entry!(start, Start);
entry!(stop, Stop);
entry!(playing, Playing);
fn state<T>(
    cx: &mut NativeCx<'_>,
    owner: ObjId,
    f: impl FnOnce(&mut State) -> T,
) -> NativeResult<T> {
    cx.heap_mut().with_native_state::<State, _>(owner, f)
}
fn initialize(cx: &mut NativeCx<'_>, args: &[Value], action: Action) -> NativeResult<NativeStep> {
    let count: usize = match action {
        Action::Open => 2,
        Action::Start => 1,
        _ => 0,
    };
    if args.len() < count {
        return Err(NativeError::Missing(count.saturating_sub(1)));
    }
    let owner = cx.this();
    extensions::layer_size(cx, Value::Obj(owner.into()))?;
    cx.heap_mut().initialize_native_default::<State>(owner)?;
    let function = cx.function().ok_or(NativeError::This)?;
    let registry = cx
        .heap_mut()
        .with_native_state::<Registry, _>(function, |s| s.clone())?;
    let (api, playback) = state(cx, owner, |s| (s.api, s.playback.clone()))?;
    {
        let mut entries = registry.0.borrow_mut();
        entries.retain(|v| v.strong_count() != 0);
        let entry = Rc::downgrade(&playback.0);
        if !entries.iter().any(|v| v.ptr_eq(&entry)) {
            entries.push(entry);
        }
    }
    let args = args[..match action {
        Action::Open => 2,
        Action::Start => 1,
        _ => 0,
    }]
        .to_vec();
    if let Some(api) = api {
        return execute(cx, owner, api, playback, args, action);
    }
    let class = cx
        .heap()
        .registered_class("Layer")
        .ok_or(NativeError::This)?;
    Box::new(Initialize {
        owner,
        class,
        playback,
        api: [Value::Void; 9],
        index: 0,
        args,
        action,
    })
    .next(cx)
}
#[derive(tjs_bind::Trace)]
struct Initialize {
    owner: ObjId,
    class: ObjId,
    playback: Shared,
    api: [Value; 9],
    index: usize,
    args: Vec<Value>,
    action: Action,
}
impl Initialize {
    fn next(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index < 9 {
            let (target, name) = if self.index < 6 {
                (self.class, PROPERTIES[self.index])
            } else {
                (self.owner, EVENTS[self.index - 6])
            };
            let key = Value::Str(
                cx.heap_mut()
                    .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
            );
            return Ok(NativeStep::GetOr {
                object: Value::Obj(ObjRef::bound(target)),
                key,
                raw: true,
                fallback: Value::Void,
                continuation: self,
            });
        }
        let tick = cx.heap_mut().alloc_native_function(tick::CALL);
        let tick = Value::Obj(ObjRef {
            object: Some(tick),
            this: Some(self.owner),
        });
        state(cx, self.owner, |s| {
            s.api = Some(self.api);
            s.tick = Some(tick);
        })?;
        execute(
            cx,
            self.owner,
            self.api,
            self.playback,
            self.args,
            self.action,
        )
    }
}
impl NativeContinuation for Initialize {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.index < 6 {
            bound(value, self.owner)?;
        } else if !matches!(value, Value::Void | Value::Obj(_)) {
            return Err(NativeError::Type("a movie event object"));
        }
        self.api[self.index] = value;
        self.index += 1;
        self.next(cx)
    }
}
fn bound(value: Value, owner: ObjId) -> NativeResult<Value> {
    let Value::Obj(mut r) = value else {
        return Err(NativeError::Type("a cached Layer property"));
    };
    if r.object.is_none() {
        return Err(NativeError::This);
    }
    r.this = Some(owner);
    Ok(Value::Obj(r))
}
fn event(
    cx: &mut NativeCx<'_>,
    owner: ObjId,
    function: Value,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if let Value::Obj(r) = function
        && let Some(id) = r.object
        && cx.heap().is_valid(id)?
        && matches!(
            cx.heap().object(id)?.kind(),
            tjs_core::ObjectKind::Function | tjs_core::ObjectKind::NativeFunction
        )
    {
        return Ok(NativeStep::CallDiscard {
            function: Value::Obj(ObjRef {
                object: Some(id),
                this: Some(owner),
            }),
            arguments: Vec::new(),
            continuation: next,
        });
    }
    Ok(NativeStep::Continue(next))
}
fn done() -> Box<dyn NativeContinuation> {
    tjs_bind::flow::complete(Value::Void)
}
fn execute(
    cx: &mut NativeCx<'_>,
    owner: ObjId,
    api: [Value; 9],
    playback: Shared,
    args: Vec<Value>,
    action: Action,
) -> NativeResult<NativeStep> {
    match action {
        Action::Open => {
            let Value::Str(path) = value::to_string(cx.heap_mut(), args[0])? else {
                unreachable!()
            };
            let path = tjs_core::string::c_string(cx.heap().string(path)?).to_vec();
            let alpha = value::to_integer(cx.heap(), args[1])? != 0;
            let generation = {
                let mut p = playback.borrow_mut();
                p.clear();
                p.alpha = alpha;
                p.busy = true;
                p.generation
            };
            extensions::open_effect_video(
                cx,
                &path,
                Box::new(Opening {
                    owner,
                    api,
                    playback,
                    generation,
                    index: 0,
                    size: [0; 2],
                    complete: false,
                }),
            )
        }
        Action::Start => {
            let looping = value::to_integer(cx.heap(), args[0])? != 0;
            let tick = state(cx, owner, |s| s.tick.unwrap())?;
            let mut p = playback.borrow_mut();
            let Some(handle) = p.handle.clone() else {
                return Ok(NativeStep::Return(Value::Void));
            };
            p.hook = None;
            p.hook = Some(extensions::continuous(cx, tick)?);
            p.generation = p.generation.wrapping_add(1);
            p.looping = looping;
            p.playing = true;
            p.busy = false;
            handle.play(true);
            drop(p);
            event(cx, owner, api[6], done())
        }
        Action::Stop => {
            let playing = {
                let mut p = playback.borrow_mut();
                let playing = p.playing;
                p.clear();
                playing
            };
            Ok(if playing {
                event(cx, owner, api[7], done())?
            } else {
                NativeStep::Return(Value::Void)
            })
        }
        Action::Playing => {
            let mut p = playback.borrow_mut();
            if p.playing && p.hook.as_ref().is_none_or(|h| !h.active()) {
                p.clear();
            }
            Ok(NativeStep::Return(Value::Int(i64::from(p.playing))))
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Opening {
    owner: ObjId,
    api: [Value; 9],
    playback: Shared,
    generation: u64,
    index: usize,
    size: [u32; 2],
    complete: bool,
}
impl Drop for Opening {
    fn drop(&mut self) {
        if !self.complete {
            let mut p = self.playback.borrow_mut();
            if p.generation == self.generation {
                p.clear();
            }
        }
    }
}
impl VideoOpened for Opening {
    fn opened(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        handle: VideoHandle,
    ) -> NativeResult<NativeStep> {
        if self.playback.borrow().generation != self.generation {
            self.complete = true;
            return Ok(NativeStep::Return(Value::Void));
        }
        let info = handle.info();
        let alpha = self.playback.borrow().alpha;
        self.size = [
            if alpha {
                info.size.width / 2
            } else {
                info.size.width
            },
            info.size.height,
        ];
        if self.size.contains(&0) {
            return Err(NativeError::Message("effect movie dimensions are empty"));
        }
        self.playback.borrow_mut().handle = Some(handle);
        self.resume(cx, Value::Void)
    }
}
impl NativeContinuation for Opening {
    fn resume(mut self: Box<Self>, _cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if self.playback.borrow().generation != self.generation {
            self.complete = true;
            return Ok(NativeStep::Return(Value::Void));
        }
        if self.index < 3 {
            let slot = [2, 3, 5][self.index];
            let value = if self.index < 2 {
                i64::from(self.size[self.index])
            } else if self.playback.borrow().alpha {
                2
            } else {
                1
            }; // ltAlpha=2, ltOpaque=1
            self.index += 1;
            return Ok(NativeStep::SetProperty {
                object: bound(self.api[slot], self.owner)?,
                key: Value::Void,
                value: Value::Int(value),
                flags: Default::default(),
                continuation: self,
            });
        }
        self.playback.borrow_mut().busy = false;
        self.complete = true;
        Ok(NativeStep::Return(Value::Void))
    }
}
#[tjs_bind::function(resumable = true)]
fn cleanup(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let (api, playback) = state(cx, owner, |s| (s.api, s.playback.clone()))?;
    let playing = {
        let mut p = playback.borrow_mut();
        let v = p.playing;
        p.clear();
        v
    };
    Ok(if playing {
        event(cx, owner, api.map_or(Value::Void, |a| a[7]), done())?
    } else {
        NativeStep::Return(Value::Void)
    })
}
#[tjs_bind::function(resumable = true)]
fn tick(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let (api, playback) = state(cx, owner, |s| (s.api.unwrap(), s.playback.clone()))?;
    let mut p = playback.borrow_mut();
    if !p.playing || p.busy {
        return Ok(NativeStep::Return(Value::Void));
    }
    let Some(handle) = p.handle.clone() else {
        p.clear();
        return Ok(NativeStep::Return(Value::Void));
    };
    if handle.error().is_some() {
        p.clear();
        drop(p);
        return event(cx, owner, api[7], done());
    }
    if let Some(frame) = handle.next_frame() {
        p.busy = true;
        let generation = p.generation;
        drop(p);
        return Box::new(Frame {
            owner,
            api,
            playback,
            generation,
            pixels: Some(frame.pixels),
            size: [0; 2],
            index: 0,
            complete: false,
        })
        .resume(cx, Value::Void);
    }
    if handle.finished() {
        if p.looping {
            p.busy = true;
            let generation = p.generation;
            drop(p);
            return extensions::rewind_video(
                cx,
                handle,
                Box::new(Frame {
                    owner,
                    api,
                    playback,
                    generation,
                    pixels: None,
                    size: [0; 2],
                    index: 4,
                    complete: false,
                }),
            );
        }
        p.clear();
        drop(p);
        return event(cx, owner, api[7], done());
    }
    Ok(NativeStep::Return(Value::Void))
}
#[derive(tjs_bind::Trace)]
struct Frame {
    owner: ObjId,
    api: [Value; 9],
    playback: Shared,
    generation: u64,
    #[trace(skip = "Decoded frame pixels own budgeted buffers without TJS handles")]
    pixels: Option<krkr_engine::protocol::pixels::VideoPixels>,
    size: [u32; 2],
    index: usize,
    complete: bool,
}
impl Drop for Frame {
    fn drop(&mut self) {
        if !self.complete {
            let mut p = self.playback.borrow_mut();
            if p.generation == self.generation {
                p.clear();
            }
        }
    }
}
impl extensions::WorkContinuation<Arc<krkr_engine::protocol::pixels::Pixels>> for Frame {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_engine::protocol::pixels::Pixels>,
    ) -> NativeResult<NativeStep> {
        self.pixels = Some(pixels.into());
        let height = Value::Int(i64::from(self.size[1]));
        NativeContinuation::resume(self, cx, height)
    }
}
impl NativeContinuation for Frame {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.playback.borrow().generation != self.generation {
            self.complete = true;
            return Ok(NativeStep::Return(Value::Void));
        }
        if self.index == 1 || self.index == 2 {
            self.size[self.index - 1] = (value::to_integer(cx.heap(), value)? as i32).max(0) as u32;
        }
        match self.index {
            0 | 1 => {
                let slot = self.index + 2;
                self.index += 1;
                Ok(NativeStep::Get {
                    object: bound(self.api[slot], self.owner)?,
                    key: Value::Void,
                    continuation: self,
                })
            }
            2 => {
                let logical = self
                    .playback
                    .borrow()
                    .handle
                    .as_ref()
                    .ok_or(NativeError::This)?
                    .info()
                    .size;
                if let Some(krkr_engine::protocol::pixels::VideoPixels::Rgba(pixels)) =
                    self.pixels.as_ref()
                    && pixels.size != logical
                {
                    let pixels = pixels.clone();
                    self.pixels.take();
                    let budget = extensions::layer_pixel_budget(cx, Value::Obj(self.owner.into()))?;
                    return extensions::run_work(
                        cx,
                        move |cancelled| {
                            let main = krkr_image::scale::expand(
                                pixels
                                    .main
                                    .as_ref()
                                    .ok_or(NativeError::Message("movie frame has no main plane"))?,
                                pixels.size,
                                logical,
                                &budget,
                                cancelled,
                            )
                            .map_err(|e| NativeError::Detail(e.to_string()))?;
                            Ok(Arc::new(krkr_engine::protocol::pixels::Pixels {
                                size: logical,
                                main: Some(main),
                                province: None,
                            }))
                        },
                        self,
                    );
                }
                extensions::layer_prepare_draw(cx, Value::Obj(self.owner.into()))?;
                let alpha = self.playback.borrow().alpha;
                let pixels = self.pixels.take().unwrap();
                self.index = 3;
                let size = krkr_engine::protocol::graphics::Size {
                    width: self.size[0],
                    height: self.size[1],
                };
                let step = match pixels {
                    krkr_engine::protocol::pixels::VideoPixels::Rgba(pixels) => {
                        extensions::layer_copy_pixels(
                            cx,
                            Value::Obj(self.owner.into()),
                            pixels,
                            alpha,
                            size,
                        )?
                    }
                    krkr_engine::protocol::pixels::VideoPixels::Yuv420(pixels) => {
                        extensions::layer_copy_yuv(
                            cx,
                            Value::Obj(self.owner.into()),
                            pixels,
                            logical,
                            alpha,
                            size,
                        )?
                    }
                };
                Ok(tjs_bind::flow::then(step, self))
            }
            3 => {
                self.index = 4;
                event(cx, self.owner, self.api[8], self)
            }
            _ => {
                self.playback.borrow_mut().busy = false;
                self.complete = true;
                Ok(NativeStep::Return(Value::Void))
            }
        }
    }
}
