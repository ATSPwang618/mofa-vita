//! Shared native player control surface. The Motion.Player variant uses the
//! same state and methods with the old-motion selection mode enabled.
use super::{manager, playback::Playback};
use std::sync::Arc;
use tjs_bind::{Array, Dictionary, IntoTjs, Utf16, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value, value};
type TimelineInfoList = Array<Vec<Dictionary<Vec<(&'static str, String)>>>>;
impl Default for Playback {
    fn default() -> Self {
        Self::new(false)
    }
}

#[tjs_bind::class(name = "Motion.EmotePlayer")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub manager: Value,
        pub tags: Value,
        pub use_d3d: bool,
        #[trace(skip = "Playback owns Rust animation data, without script handles")]
        pub playback: Playback,
        #[trace(skip = "Owned transform contains no VM handles")]
        pub transform: super::super::transform::Transform,
        #[trace(skip = "Renderer owns Rust buffers and animation snapshots only")]
        pub drawing: super::super::drawing::Drawing,
    }
    impl State {
        #[tjs::method(name = "skipToSync")]
        fn skip_to_sync(&mut self) {
            self.playback.skip_to_sync();
        }
        #[tjs::method(name = "getLayerGetter", resumable = true)]
        fn layer_getter(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
            super::super::motion_object::get(cx, name, false)
        }
        #[tjs::method(name = "getLayerMotion", resumable = true)]
        fn layer_motion(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
            super::super::motion_object::get(cx, name, true)
        }
        // These entry points are explicitly empty in emoteplayerclass.cpp.
        // Retain the reference call surface without inventing physics effects.
        #[tjs::method(name = "initPhysics")]
        fn init_physics(&self, _metadata: Value) {}
        #[tjs::method]
        fn assign(&self, _other: Value) {}
        #[tjs::method(name = "setColor")]
        fn color(&self, #[tjs(coerce)] _color: i64) {}
        #[tjs::method(name = "setOuterForce")]
        fn force(&self, _name: Utf16, _x: f64, _y: f64) {}
        #[tjs::method(name = "startWind")]
        fn wind(&self, _start: f64, _goal: f64, _speed: f64, _min: f64, _max: f64) {}
        #[tjs::method(name = "stopWind")]
        fn stop_wind(&self) {}
        #[tjs::method]
        fn skip(&self) {}
        #[tjs::method]
        fn pass(&self) {}
        #[tjs::method(name = "setSlant")]
        fn slant(&self, _x: f64, _y: f64) {}
        #[tjs::method(name = "setTimelineBlendRatio")]
        fn timeline_blend(&self, _name: Utf16, _ratio: f64, _time: f64, _easing: f64) {}
        #[tjs::getter(name = "outline")]
        fn outline(&self) -> Value {
            Value::Void
        }
        #[tjs::setter(name = "outline")]
        fn set_outline(&self, _input: Value) {}
        #[tjs::method(name = "setFlip")]
        fn flip(&mut self, flip: bool) {
            if flip {
                self.transform.zoom[1] = -self.transform.zoom[1];
            }
            // Reference defers the matrix update until a transform setter.
        }
        #[tjs::method(resumable = true)]
        fn contains(
            cx: &mut NativeCx<'_>,
            args: tjs_bind::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            super::super::query::contains(cx, args)
        }
        #[tjs::method(name = "getVariableFrameList", resumable = true)]
        fn variable_frames(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
            super::super::query::variable_frames(cx, name)
        }
        #[tjs::method]
        fn clear(
            cx: &mut NativeCx<'_>,
            layer: Value,
            #[tjs(coerce)] _neutral_color: i64,
        ) -> NativeResult<()> {
            if let Ok(id) = crate::exports::object(layer)
                && super::super::adaptor::bindings::with_state(cx, id, |s| {
                    s.target = None;
                    s.generation = s.generation.wrapping_add(1);
                })
                .is_ok()
            {
                return Ok(());
            }
            if krkr_engine::extensions::layer_size(cx, layer).is_err() {
                return Ok(());
            }
            cx.with_state::<Self, _>(|s, _| {
                s.drawing.target = None;
                s.drawing.generation = s.drawing.generation.wrapping_add(1);
                Ok(())
            })
        }
        #[tjs::method]
        fn stop(&mut self) {
            self.playback.stopped = true;
            self.playback.playing = false;
            self.playback.all_playing = false;
        }
        #[tjs::method(name = "getCommandList")]
        fn commands(&self) -> Array<Vec<i64>> {
            Array(vec![i64::from(self.drawing.ping_pong)])
        }
        #[tjs::getter(name = "useD3D")]
        fn d3d(&self) -> bool {
            self.use_d3d
        }
        #[tjs::setter(name = "useD3D")]
        fn set_d3d(&mut self, v: bool) {
            self.use_d3d = v;
        }
        #[tjs::method(name = "fadeInTimeline")]
        fn fade_in(&mut self, name: Utf16, _time: f64, _easing: f64) {
            self.play_timeline(name, 0);
        }
        #[tjs::method(name = "fadeOutTimeline")]
        fn fade_out(&mut self, name: Utf16, _time: f64, _easing: f64) {
            self.stop_timeline(name);
        }
        #[tjs::method(name = "getPlayingTimelineInfoList")]
        fn timeline_info(&self) -> TimelineInfoList {
            let entries = self
                .playback
                .main
                .as_ref()
                .and_then(|n| self.playback.files.get(n))
                .map(|s| {
                    s.timelines
                        .iter()
                        .map(|&i| {
                            Dictionary(vec![("label", s.file.metadata.timelines[i].label.clone())])
                        })
                        .collect()
                })
                .unwrap_or_default();
            Array(entries)
        }
        #[tjs::method]
        fn progress(&mut self, milliseconds: f64) {
            if self.playback.advance(
                milliseconds,
                self.transform.size.map(f64::from),
                self.transform.origin.map(f64::from),
            ) {
                let mut transform = self.transform.clone();
                if self
                    .playback
                    .main
                    .as_ref()
                    .and_then(|n| self.playback.files.get(n))
                    .is_some_and(|s| s.file.metadata.mirror)
                    && let Some(root) = transform.root.as_mut()
                {
                    root.attach = root
                        .attach
                        .multiply(super::super::mesh::Matrix::scale(-1., 1., 1.));
                }
                self.drawing.snapshot = Some(Arc::new((self.playback.clone(), transform)));
                self.drawing.ping_pong = !self.drawing.ping_pong;
                self.drawing.generation = self.drawing.generation.wrapping_add(1);
            }
        }
        #[tjs::method(resumable = true)]
        fn draw(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
            super::super::drawing::start(cx, layer)
        }
        #[tjs::method(name = "setCoord")]
        fn coord(&mut self, x: f64, y: f64) {
            self.transform.coord[0] = x as f32;
            self.transform.coord[1] = y as f32;
            self.transform.update();
        }
        #[tjs::method(name = "setScale")]
        fn scale(&mut self, v: f64) {
            self.transform.zoom = [v as f32; 2];
            self.transform.update();
        }
        #[tjs::method(name = "setZoom")]
        fn zoom(&mut self, x: f64, y: f64) {
            self.transform.zoom = [x as f32, y as f32];
            self.transform.update();
        }
        #[tjs::method(name = "setRotate")]
        fn rotate(&mut self, v: f64) {
            self.transform.angle = v as f32;
            self.transform.update();
        }
        #[tjs::method(name = "setCameraOffset")]
        fn camera(&mut self, #[tjs(coerce)] x: i32, #[tjs(coerce)] y: i32) {
            self.transform.camera = [x as f32, y as f32];
        }
        #[tjs::method(name = "setDrawAffineTranslateMatrix")]
        fn affine(
            &mut self,
            a: f64,
            b: f64,
            c: f64,
            d: f64,
            #[tjs(coerce)] x: i32,
            #[tjs(coerce)] y: i32,
        ) {
            if self.playback.main.is_some() {
                self.transform.affine = super::super::mesh::Matrix([
                    a as f32, -c as f32, 0., 0., -b as f32, d as f32, 0., 0., 0., 0., 1., 0.,
                    x as f32, y as f32, 0., 1.,
                ]);
                self.transform.update();
            }
        }
        #[tjs::method]
        fn serialize(&self) -> Dictionary<Vec<(&'static str, f64)>> {
            Dictionary(
                super::super::transform::FIELDS
                    .into_iter()
                    .zip(self.transform.serialized())
                    .collect(),
            )
        }
        #[tjs::method(resumable = true)]
        fn unserialize(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            if !matches!(input,Value::Obj(r) if r.object.is_some()) {
                return Ok(NativeStep::Return(Value::Void));
            }
            super::Restore {
                owner: cx.this(),
                input,
                index: 0,
            }
            .next(cx)
        }

        pub(super) fn refresh(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let manager = crate::exports::object(self.manager)?;
            let files = manager::bindings::with_state(cx, manager, |s| {
                s.cache
                    .iter()
                    .map(|(name, e)| (name.clone(), e.resource.file.clone()))
                    .collect::<Vec<_>>()
            })?;
            self.playback.refresh(files).map_err(NativeError::Message)
        }
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, manager: Value) -> NativeResult<Self> {
            super::new(cx, manager, false)
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.drawing = super::super::drawing::Drawing {
                generation: self.drawing.generation.wrapping_add(1),
                ..Default::default()
            };
            self.transform = Default::default();
            self.playback = Playback::new(self.playback.old_motion);
            self.manager = Value::Void;
            self.tags = Value::Void;
        }
        #[tjs::method]
        fn play(
            cx: &mut NativeCx<'_>,
            name: Utf16,
            #[tjs(coerce)] _flags: i32,
        ) -> NativeResult<()> {
            let name = String::from_utf16_lossy(&name.0);
            cx.with_state::<Self, _>(|s, cx| {
                s.refresh(cx)?;
                s.playback.play(name);
                s.drawing.snapshot = None;
                s.drawing.generation = s.drawing.generation.wrapping_add(1);
                Ok(())
            })
        }
        #[tjs::getter(name = "motionKey")]
        fn motion_key(&self) -> Utf16 {
            Utf16(self.playback.motion_key.clone())
        }
        #[tjs::setter(name = "motionKey")]
        fn set_motion_key(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<()> {
            cx.with_state::<Self, _>(|s, cx| {
                s.refresh(cx)?;
                s.playback.set_motion_key(name.0);
                Ok(())
            })
        }
        #[tjs::getter(name = "motion")]
        fn motion(&self) -> String {
            self.playback.motion.clone()
        }
        #[tjs::setter(name = "motion")]
        fn set_motion(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<()> {
            Self::play(cx, name, 0)
        }
        #[tjs::getter(name = "chara")]
        fn character(&self) -> String {
            self.playback.character.clone()
        }
        #[tjs::setter(name = "chara")]
        fn set_character(&mut self, name: Utf16) {
            let name = String::from_utf16_lossy(&name.0);
            self.playback.character = name;
        }
        #[tjs::getter(name = "playing")]
        fn playing(&self) -> bool {
            self.playback.playing
        }
        #[tjs::setter(name = "playing")]
        fn set_playing(&mut self, v: bool) {
            self.playback.playing = v;
        }
        #[tjs::getter(name = "animating")]
        fn animating(&self) -> bool {
            self.playback.playing
        }
        #[tjs::setter(name = "animating")]
        fn set_animating(&mut self, v: bool) {
            self.playback.playing = v;
        }
        #[tjs::getter(name = "allplaying")]
        fn all_playing(&self) -> bool {
            self.playback.all_playing
        }
        #[tjs::setter(name = "allplaying")]
        fn set_all_playing(&mut self, v: bool) {
            self.playback.all_playing = v;
        }
        #[tjs::getter(name = "tickCount")]
        fn tick(&self) -> f64 {
            self.playback.clock
        }
        #[tjs::setter(name = "tickCount")]
        fn set_tick(&mut self, v: f64) {
            self.playback.clock = v;
        }
        #[tjs::getter(name = "speed")]
        fn speed(&self) -> f64 {
            self.playback.speed
        }
        #[tjs::setter(name = "speed")]
        fn set_speed(&mut self, v: f64) {
            self.playback.speed = v;
        }
        #[tjs::getter(name = "loopTime")]
        fn loop_time(&self) -> i64 {
            self.playback.loop_time().into()
        }
        #[tjs::setter(name = "loopTime")]
        fn set_loop_time(&mut self, #[tjs(coerce)] _v: i32) -> NativeResult<()> {
            Err(NativeError::Message("reject to set loopTime"))
        }
        #[tjs::getter(name = "variableKeys")]
        fn variable_keys(&self) -> Array<Vec<String>> {
            Array(
                self.playback
                    .main
                    .as_ref()
                    .and_then(|n| self.playback.files.get(n))
                    .map_or_else(Vec::new, |s| s.variables.keys().cloned().collect()),
            )
        }
        #[tjs::setter(name = "variableKeys")]
        fn set_variable_keys(&mut self, _v: Value) -> NativeResult<()> {
            Err(NativeError::Message("reject to set variableKeys"))
        }
        #[tjs::getter(name = "tags")]
        fn tags(&self) -> Value {
            self.tags
        }
        #[tjs::setter(name = "tags")]
        fn set_tags(&mut self, v: Value) {
            self.tags = v;
        }
        #[tjs::method(name = "setVariable")]
        fn variable_set(&mut self, name: Utf16, value: f64) -> NativeResult<()> {
            let name = String::from_utf16_lossy(&name.0);
            if self.playback.main.is_some() {
                self.playback
                    .set_variable(&name, value)
                    .map_err(NativeError::Message)?;
            }
            Ok(())
        }
        #[tjs::method(name = "getVariable")]
        fn variable(&self, name: Utf16) -> f64 {
            let name = String::from_utf16_lossy(&name.0);
            self.playback.variable(&name)
        }
        #[tjs::method(name = "playTimeline")]
        fn play_timeline(&mut self, name: Utf16, #[tjs(default = 0, coerce)] _flags: i32) {
            let name = String::from_utf16_lossy(&name.0);
            if let Some(state) = self
                .playback
                .main
                .as_ref()
                .and_then(|n| self.playback.files.get_mut(n))
            {
                state.start_timeline(&name);
            }
        }
        #[tjs::method(name = "stopTimeline")]
        fn stop_timeline(&mut self, name: Utf16) {
            let name = String::from_utf16_lossy(&name.0);
            if let Some(state) = self
                .playback
                .main
                .as_ref()
                .and_then(|n| self.playback.files.get_mut(n))
            {
                state.stop_timeline(&name);
            }
        }
        #[tjs::method(name = "getTimelinePlaying")]
        fn timeline_playing(&self, name: Utf16) -> bool {
            let name = String::from_utf16_lossy(&name.0);
            self.playback
                .main
                .as_ref()
                .and_then(|n| self.playback.files.get(n))
                .is_some_and(|s| {
                    s.timelines
                        .iter()
                        .any(|&i| s.file.metadata.timelines[i].label == name)
                })
        }
        #[tjs::method(name = "getLoopTimeline")]
        fn timeline_loop(&self, name: Utf16) -> bool {
            let name = String::from_utf16_lossy(&name.0);
            self.playback
                .main
                .as_ref()
                .and_then(|n| self.playback.files.get(n))
                .and_then(|s| s.file.metadata.timelines.iter().find(|t| t.label == name))
                .is_some_and(|t| t.last_time < 0)
        }
        #[tjs::method(name = "getTimelineTotalFrameCount")]
        fn timeline_length(&self, name: Utf16) -> f64 {
            let name = String::from_utf16_lossy(&name.0);
            self.playback
                .main
                .as_ref()
                .and_then(|n| self.playback.files.get(n))
                .and_then(|s| s.file.metadata.timelines.iter().find(|t| t.label == name))
                .map_or(0., |t| {
                    (i64::from(t.loop_end) - i64::from(t.loop_begin) + 1) as f64
                })
        }
        fn timelines(&self, diff: i8) -> Array<Vec<String>> {
            Array(
                self.playback
                    .main
                    .as_ref()
                    .and_then(|n| self.playback.files.get(n))
                    .map_or_else(Vec::new, |s| {
                        s.file
                            .metadata
                            .timelines
                            .iter()
                            .filter(|t| t.diff == diff)
                            .map(|t| t.label.clone())
                            .collect()
                    }),
            )
        }
        #[tjs::method(name = "getMainTimelineLabelList")]
        fn main_timelines(&self) -> Array<Vec<String>> {
            self.timelines(0)
        }
        #[tjs::method(name = "getDiffTimelineLabelList")]
        fn diff_timelines(&self) -> Array<Vec<String>> {
            self.timelines(1)
        }
    }
}
fn new(cx: &mut NativeCx<'_>, manager: Value, old_motion: bool) -> NativeResult<bindings::State> {
    let mut state = bindings::State {
        manager,
        playback: Playback::new(old_motion),
        ..Default::default()
    };
    state.refresh(cx)?;
    Ok(state)
}
#[tjs_bind::function]
fn old_player(cx: &mut NativeCx<'_>, manager: Value) -> NativeResult<Value> {
    let state = new(cx, manager, true)?;
    cx.construct(state)
}
pub(super) static PLAYER: tjs_core::NativeClass = tjs_core::NativeClass {
    name: "Motion.Player",
    constructor: old_player::CALL,
    ..bindings::CLASS
};

#[derive(tjs_bind::Trace)]
struct Restore {
    owner: ObjId,
    input: Value,
    index: usize,
}
impl Restore {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let Some(name) = super::transform::FIELDS.get(self.index) else {
            return Ok(NativeStep::Return(Value::Void));
        };
        let key = name.to_string().into_tjs(cx.heap_mut())?;
        let fallback =
            bindings::with_state(cx, self.owner, |s| s.transform.serialized()[self.index])?;
        Ok(NativeStep::GetOr {
            object: self.input,
            key,
            raw: false,
            fallback: Value::Real(fallback),
            continuation: flow::callback(self, |mut s, cx, v| {
                let value = value::to_real(cx.heap(), v)?;
                bindings::with_state(cx, s.owner, |state| {
                    state.transform.restore_field(s.index, value)
                })?;
                s.index += 1;
                s.next(cx)
            }),
        })
    }
}
