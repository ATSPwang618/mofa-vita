//! Font selection follows the SDL plugin's private-face list, with the engine's
//! portable font service supplying file, system and bundled fallback faces.
use ab_glyph::Font as _;
use krkr_engine::{
    extensions::{self, FontFace},
    protocol::text::Font,
    storages,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};
use tjs_bind::{Array, IntoTjs, Utf16};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value};

type Faces = Vec<(Vec<u16>, Arc<FontFace>)>;
#[derive(Clone, Default)]
pub(super) struct Registry {
    faces: Rc<RefCell<Faces>>,
    generation: Rc<Cell<u64>>,
}
impl Trace for Registry {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Registry {
    pub fn clear(&self) {
        self.faces.borrow_mut().clear();
        self.generation.set(self.generation.get().wrapping_add(1));
    }
    fn face(&self, name: &[u16]) -> Option<Arc<FontFace>> {
        let faces = self.faces.borrow();
        faces
            .iter()
            .find(|e| e.0 == name)
            .or_else(|| faces.first())
            .map(|e| e.1.clone())
    }
}
fn registry(cx: &mut NativeCx<'_>) -> NativeResult<Registry> {
    let class = cx
        .heap()
        .registered_class("GdiPlus.Font")
        .ok_or(NativeError::This)?;
    bindings::with_state(cx, class, |s| s.registry.clone())
}
#[derive(tjs_bind::Trace)]
enum After {
    Add,
    Construct(bindings::State),
    Set(ObjId),
}
#[derive(tjs_bind::Trace)]
struct Loading {
    registry: Registry,
    generation: u64,
    name: Vec<u16>,
    after: After,
}
impl extensions::WorkContinuation<Arc<FontFace>> for Loading {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        face: Arc<FontFace>,
    ) -> NativeResult<NativeStep> {
        if self.generation != self.registry.generation.get() {
            return Err(NativeError::Message(
                "font plugin was unloaded during loading",
            ));
        }
        self.registry.faces.borrow_mut().push((self.name, face));
        finish(cx, self.after)
    }
}
fn finish(cx: &mut NativeCx<'_>, after: After) -> NativeResult<NativeStep> {
    match after {
        After::Construct(state) => cx.construct(state).map(NativeStep::Return),
        After::Set(owner) => {
            // A suspended setter must still validate its original receiver.
            bindings::with_state(cx, owner, |_| ())?;
            Ok(NativeStep::Return(Value::Void))
        }
        After::Add => Ok(NativeStep::Return(Value::Void)),
    }
}
fn load(
    cx: &mut NativeCx<'_>,
    registry: Registry,
    name: Vec<u16>,
    after: After,
) -> NativeResult<NativeStep> {
    storages::managed::plans(
        cx,
        vec![(name.clone(), false)],
        Loading {
            generation: registry.generation.get(),
            registry,
            name,
            after,
        },
        |s, cx, mut plans| {
            let plan = plans.pop().flatten();
            let font = Font {
                face: String::from_utf16_lossy(&s.name),
                file: plan.is_some(),
                ..Font::default()
            };
            extensions::resolve_font_source(cx, font, plan, Box::new(s))
        },
    )
}
#[tjs_bind::function(resumable = true)]
pub(super) fn add(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
    let registry = registry(cx)?;
    load(cx, registry, name.0, After::Add)
}
#[derive(tjs_bind::Trace)]
struct Listing(Registry);
impl extensions::WorkContinuation<Vec<String>> for Listing {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        names: Vec<String>,
    ) -> NativeResult<NativeStep> {
        let mut names: Vec<_> = names
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(|n| Utf16(n.encode_utf16().collect()))
            .collect();
        names.extend(
            self.0
                .faces
                .borrow()
                .iter()
                .filter(|e| !e.0.is_empty())
                .map(|e| Utf16(e.0.clone())),
        );
        Ok(NativeStep::Return(Array(names).into_tjs(cx.heap_mut())?))
    }
}
#[tjs_bind::function(resumable = true)]
pub(super) fn list(cx: &mut NativeCx<'_>, private_only: bool) -> NativeResult<NativeStep> {
    let listing = Box::new(Listing(registry(cx)?));
    if private_only {
        extensions::WorkContinuation::resume(listing, cx, Vec::new())
    } else {
        extensions::list_font_families(cx, listing)
    }
}

#[tjs_bind::class(name = "GdiPlus.Font")]
pub(super) mod bindings {
    use super::*;
    #[derive(Clone)]
    pub struct State {
        pub(super) registry: Registry,
        pub(super) name: Vec<u16>,
        pub(in crate::layer_draw) size: f64,
        style: i32,
        force: bool,
        metrics: Cell<Option<[f64; 3]>>,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                registry: Registry::default(),
                name: Vec::new(),
                size: 12.,
                style: 0,
                force: false,
                metrics: Cell::new(None),
            }
        }
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        pub(in crate::layer_draw) fn face(&self) -> Option<Arc<FontFace>> {
            self.registry.face(&self.name)
        }
        pub(in crate::layer_draw) fn metrics(&self) -> [f64; 3] {
            if let Some(v) = self.metrics.get() {
                return v;
            }
            let v = self.face().map_or(
                [self.size * 0.8, self.size * 0.2, self.size * 1.2],
                |face| {
                    let f = &face.font;
                    let scale = self.size as f32 / f.units_per_em().unwrap_or(1.);
                    [
                        (f.ascent_unscaled() * scale) as f64,
                        (-f.descent_unscaled() * scale) as f64,
                        (f.line_gap_unscaled() * scale) as f64,
                    ]
                },
            );
            self.metrics.set(Some(v));
            v
        }
        #[tjs::constructor(resumable = true)]
        fn construct(
            cx: &mut NativeCx<'_>,
            name: Utf16,
            size: f64,
            #[tjs(coerce)] style: i32,
        ) -> NativeResult<NativeStep> {
            let registry = registry(cx)?;
            let state = Self {
                registry: registry.clone(),
                name: name.0.clone(),
                size,
                style,
                ..Self::default()
            };
            if state.face().is_some() {
                cx.construct(state).map(NativeStep::Return)
            } else {
                load(cx, registry, name.0, After::Construct(state))
            }
        }
        #[tjs::getter(name = "familyName")]
        fn name(&self) -> Utf16 {
            Utf16(self.name.clone())
        }
        #[tjs::setter(name = "familyName", resumable = true)]
        fn set_name(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
            let owner = cx.this();
            let (registry, missing) = with_state(cx, owner, |s| {
                s.name = name.0.clone();
                s.metrics.set(None);
                (s.registry.clone(), s.face().is_none())
            })?;
            if missing {
                load(cx, registry, name.0, After::Set(owner))
            } else {
                finish(cx, After::Set(owner))
            }
        }
        #[tjs::getter(name = "emSize")]
        fn size(&self) -> f64 {
            self.size
        }
        #[tjs::setter(name = "emSize")]
        fn set_size(&mut self, v: f64) {
            self.size = v;
            self.metrics.set(None);
        }
        #[tjs::getter(name = "style")]
        fn style(&self) -> i64 {
            self.style.into()
        }
        #[tjs::setter(name = "style")]
        fn set_style(&mut self, #[tjs(coerce)] v: i32) {
            self.style = v;
            self.metrics.set(None);
        }
        #[tjs::getter(name = "forceSelfPathDraw")]
        fn force(&self) -> bool {
            self.force
        }
        #[tjs::setter(name = "forceSelfPathDraw", resumable = true)]
        fn set_force(cx: &mut NativeCx<'_>, v: bool) -> NativeResult<NativeStep> {
            let name = with_state(cx, cx.this(), |s| {
                s.force = v;
                Utf16(s.name.clone())
            })?;
            Self::set_name(cx, name)
        }
        #[tjs::getter(name = "ascent")]
        fn ascent(&self) -> f64 {
            self.metrics()[0]
        }
        #[tjs::getter(name = "descent")]
        fn descent(&self) -> f64 {
            self.metrics()[1]
        }
        #[tjs::getter(name = "lineSpacing")]
        fn spacing(&self) -> f64 {
            self.metrics()[2]
        }
        #[tjs::getter(name = "ascentLeading")]
        fn ascent_leading(&self) -> f64 {
            self.metrics();
            0.
        }
        #[tjs::getter(name = "descentLeading")]
        fn descent_leading(&self) -> f64 {
            self.metrics();
            0.
        }
    }
}
pub(super) fn snapshot(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<bindings::State> {
    let owner = crate::exports::object(value)?;
    bindings::with_state(cx, owner, |s| s.clone())
}

pub(super) fn install(heap: &mut tjs_core::Heap, registry: Registry) -> NativeResult<ObjId> {
    let mut state = bindings::State::default();
    state.registry = registry;
    bindings::install_with_state(heap, state)
}
