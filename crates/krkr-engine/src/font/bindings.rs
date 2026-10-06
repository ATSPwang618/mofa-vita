use super::tasks::{Action, Query};
use super::*;
pub(crate) use implementation::State;
use tjs_core::{NativeStep, RestArgs, value};
fn units(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), v)? else {
        unreachable!()
    };
    Ok(krkr_assets::name::c_string(cx.heap().string(id)?).to_vec())
}
#[tjs_bind::class(name = "Font")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        pub target: Target,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            if let Target::Layer(link) = &self.target {
                link.trace(visit);
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn create(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Self> {
            let service = service(cx)?;
            let target = if let Some(&layer) = args.first() {
                Target::Layer(crate::layer::font_link(cx.heap_mut(), layer)?)
            } else {
                Target::default()
            };
            Ok(Self {
                service: Some(service),
                target,
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(name = "addFont", class_only = true, resumable = true)]
        fn add_font(cx: &mut NativeCx<'_>, storage: Value) -> NativeResult<NativeStep> {
            let storage = units(cx, storage)?;
            super::super::tasks::register_names(cx, &storage)
        }
        #[tjs::getter(name = "face")]
        fn get_face(cx: &NativeCx<'_>) -> NativeResult<String> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.face.clone())
                })?
        }
        #[tjs::setter(name = "face")]
        fn set_face(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = String::from_utf16_lossy(&units(cx, v)?);
            self.target.update(|f| f.face = v)
        }
        #[tjs::getter(name = "height")]
        fn get_height(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.height.into())
                })?
        }
        #[tjs::setter(name = "height")]
        fn set_height(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = value::to_integer(cx.heap(), v)? as i32;
            self.target.update(|f| f.height = v.saturating_abs())
        }
        #[tjs::getter(name = "angle")]
        fn get_angle(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.angle.into())
                })?
        }
        #[tjs::setter(name = "angle")]
        fn set_angle(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = value::to_integer(cx.heap(), v)? as i32;
            self.target.update(|f| f.angle = v.rem_euclid(3600))
        }
        #[tjs::getter(name = "bold")]
        fn get_bold(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.bold)
                })?
        }
        #[tjs::setter(name = "bold")]
        fn set_bold(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = v.truthy(cx.heap())?;
            self.target.update(|f| f.bold = v)
        }
        #[tjs::getter(name = "italic")]
        fn get_italic(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.italic)
                })?
        }
        #[tjs::setter(name = "italic")]
        fn set_italic(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = v.truthy(cx.heap())?;
            self.target.update(|f| f.italic = v)
        }
        #[tjs::getter(name = "strikeout")]
        fn get_strikeout(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.strikeout)
                })?
        }
        #[tjs::setter(name = "strikeout")]
        fn set_strikeout(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = v.truthy(cx.heap())?;
            self.target.update(|f| f.strikeout = v)
        }
        #[tjs::getter(name = "underline")]
        fn get_underline(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.underline)
                })?
        }
        #[tjs::setter(name = "underline")]
        fn set_underline(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = v.truthy(cx.heap())?;
            self.target.update(|f| f.underline = v)
        }
        #[tjs::getter(name = "faceIsFileName")]
        fn get_file(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.target.read(|font| font.file)
                })?
        }
        #[tjs::setter(name = "faceIsFileName")]
        fn set_file(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            let v = v.truthy(cx.heap())?;
            self.target.update(|f| f.file = v)
        }
        #[tjs::method(name = "getTextWidth", resumable = true)]
        fn width(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, false), Query::Width)
        }
        #[tjs::method(name = "getTextHeight", resumable = true)]
        fn height(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, false), Query::Height)
        }
        #[tjs::method(name = "getEscWidthX", resumable = true)]
        fn width_x(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, false), Query::WidthX)
        }
        #[tjs::method(name = "getEscWidthY", resumable = true)]
        fn width_y(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, false), Query::WidthY)
        }
        #[tjs::method(name = "getEscHeightX", resumable = true)]
        fn height_x(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, false), Query::HeightX)
        }
        #[tjs::method(name = "getEscHeightY", resumable = true)]
        fn height_y(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, false), Query::HeightY)
        }
        #[tjs::method(name = "getGlyphDrawRect", resumable = true)]
        fn bounds(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let text = units(cx, text)?;
            self.query(cx, Action::Measure(text, true), Query::Bounds)
        }
        #[tjs::method(name = "getList", resumable = true)]
        fn list(&self, cx: &mut NativeCx<'_>, flags: Value) -> NativeResult<NativeStep> {
            let flags = value::to_integer(cx.heap(), flags)? as u32;
            self.query(cx, Action::List(flags), Query::List)
        }
        #[tjs::method(name = "mapPrerenderedFont", resumable = true)]
        fn map(cx: &mut NativeCx<'_>, storage: Value) -> NativeResult<NativeStep> {
            let storage = units(cx, storage)?;
            // File/XP3 plans can resolve inline. Acquire State in the callback
            // only: a receiver would keep it taken out during that callback.
            crate::storages::managed::plans(cx, vec![(storage, true)], (), |_, cx, mut plans| {
                let plan = plans
                    .pop()
                    .flatten()
                    .expect("required prerendered font plan");
                cx.with_state::<State, _>(|state, cx| {
                    state.query(cx, Action::Map(plan), Query::Done)
                })
            })
        }
        #[tjs::method(name = "unmapPrerenderedFont", resumable = true)]
        fn unmap(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.query(cx, Action::Unmap, Query::Done)
        }
    }
}
pub(super) fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    implementation::install(heap)
}
