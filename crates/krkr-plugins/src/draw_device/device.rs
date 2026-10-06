use tjs_bind::{Array, Dictionary, IntoTjs, Utf16};
use tjs_core::{NativeCx, NativeResult, NativeStep, Value};

#[tjs_bind::class(name = "DrawDeviceD3D")]
pub(crate) mod bindings {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        pub width: i32,
        pub height: i32,
        pub layers: Vec<Value>,
        pub window: Value,
        pub primary: Vec<Value>,
        pub manager_images: Vec<(tjs_core::ObjId, krkr_engine::extensions::GpuImage)>,
        pub clear_color: u32,
        pub manager_index: i32,
        pub stretch: i32,
        pub bicubic: f64,
        pub trans_state: f64,
        pub mask: i32,
        pub screen: [i32; 4],
        pub offset: [i32; 2],
        #[trace(skip = "Managed GPU image lease")]
        pub composite: Option<krkr_engine::extensions::GpuImage>,
        pub generation: u64,
        #[trace(skip = "Immutable transition snapshot")]
        pub previous: Option<krkr_engine::extensions::GpuImage>,
        pub transition_active: bool,
        pub transition_progress: f64,
        pub page: i32,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                width: 0,
                height: 0,
                layers: Vec::new(),
                window: Value::Void,
                primary: Vec::new(),
                manager_images: Vec::new(),
                clear_color: 0xff000000,
                manager_index: 0,
                stretch: 0,
                bicubic: 0.5,
                trans_state: 1.,
                mask: 0,
                screen: [0; 4],
                offset: [0; 2],
                composite: None,
                generation: 0,
                previous: None,
                transition_active: false,
                transition_progress: 1.,
                page: 1,
            }
        }
    }
    impl State {
        #[tjs::getter(name = "interface")]
        fn interface(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            let owner = cx.this();
            krkr_engine::extensions::register_draw_device(cx.heap_mut(), owner, super::attach::CALL)
        }
        #[tjs::method(resumable = true)]
        fn capture(
            cx: &mut NativeCx<'_>,
            layer: Value,
            #[tjs(coerce)] index: i32,
        ) -> NativeResult<NativeStep> {
            let owner = cx.this();
            super::refresh_roots(cx, owner)?;
            if krkr_engine::extensions::layer_size(cx, layer).is_err() {
                return Ok(NativeStep::Return(Value::Void));
            }
            let index = if index >= 5_000_000 {
                index - 5_000_000
            } else {
                index
            };
            let source = cx.with_state::<Self, _>(|s, _| {
                Ok(usize::try_from(index)
                    .ok()
                    .and_then(|i| s.primary.get(i).copied()))
            })?;
            let Some(source) = source else {
                return Ok(NativeStep::Return(Value::Void));
            };
            krkr_engine::extensions::layer_capture_subtree(cx, layer, source)
        }
        #[tjs::method(name = "startTransition")]
        fn start_transition(&mut self, options: Value) {
            if !matches!(options, Value::Obj(r) if r.object.is_some()) || self.composite.is_none() {
                return;
            }
            self.previous = self.composite.clone();
            self.transition_active = true;
            self.transition_progress = 0.;
            self.trans_state = 1.;
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::method(name = "stopTransition")]
        pub(crate) fn stop_transition(&mut self) {
            self.transition_active = false;
            self.transition_progress = 1.;
            self.trans_state = 1.;
            if self.previous.take().is_some() {
                self.page = if self.page == 1 { 2 } else { 1 };
            }
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::method(name = "setPrimarySize")]
        fn primary_size(&mut self, #[tjs(coerce)] w: i32, #[tjs(coerce)] h: i32) {
            self.width = w;
            self.height = h;
            self.composite = None;
            self.manager_images.clear();
            self.stop_transition();
        }
        #[tjs::method]
        fn recreate(&mut self) {
            self.composite = None;
            self.manager_images.clear();
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::method]
        fn finalize(&self) {} // Native destruction owns cleanup in the reference.
        #[tjs::method(name = "checkEnable")]
        fn check(&self, name: Utf16) -> bool {
            name.0 == "emote".encode_utf16().collect::<Vec<_>>()
        }
        #[tjs::method(name = "getModule")]
        fn module(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<Value> {
            if name.0 != "emote".encode_utf16().collect::<Vec<_>>() {
                return Ok(Value::Void);
            }
            let mask = cx.with_state::<Self, _>(|s, _| Ok(i64::from(s.mask)))?;
            Dictionary(vec![("maskMode", mask)]).into_tjs(cx.heap_mut())
        }
        #[tjs::method(resumable = true)]
        fn update(cx: &mut NativeCx<'_>, diff: f64) -> NativeResult<NativeStep> {
            super::super::compose::start(cx, diff)
        }
        #[tjs::constructor]
        fn new(#[tjs(coerce)] width: i32, #[tjs(coerce)] height: i32) -> Self {
            Self {
                width,
                height,
                screen: [0, 0, width, height],
                ..Default::default()
            }
        }
        #[tjs::getter]
        fn width(&self) -> i64 {
            i64::from(self.width)
        }
        #[tjs::setter(name = "width")]
        fn set_width(&mut self, #[tjs(coerce)] v: i32) {
            self.width = v;
        }
        #[tjs::getter]
        fn height(&self) -> i64 {
            i64::from(self.height)
        }
        #[tjs::setter(name = "height")]
        fn set_height(&mut self, #[tjs(coerce)] v: i32) {
            self.height = v;
        }
        #[tjs::method(name = "setSize")]
        fn size(&mut self, #[tjs(coerce)] w: i32, #[tjs(coerce)] h: i32) {
            self.width = w;
            self.height = h;
        }
        #[tjs::getter(name = "children")]
        fn children(&self) -> Array<Vec<Value>> {
            Array(self.layers.clone())
        }
        #[tjs::getter(name = "primaryLayers")]
        fn primary(cx: &mut NativeCx<'_>) -> NativeResult<Array<Vec<Value>>> {
            let owner = cx.this();
            super::refresh_roots(cx, owner)?;
            with_state(cx, owner, |s| Array(s.primary.clone()))
        }
        #[tjs::getter(name = "clearColor")]
        fn color(&self) -> i64 {
            i64::from(self.clear_color)
        }
        #[tjs::setter(name = "clearColor")]
        fn set_color(&mut self, #[tjs(coerce)] v: i64) {
            self.clear_color = v as u32;
        }
        #[tjs::getter(name = "layerManagerIndex")]
        fn manager(&self) -> i64 {
            i64::from(self.manager_index)
        }
        #[tjs::setter(name = "layerManagerIndex")]
        fn set_manager(cx: &mut NativeCx<'_>, #[tjs(coerce)] v: i32) -> NativeResult<()> {
            let owner = cx.this();
            super::refresh_roots(cx, owner)?;
            let (window, index) = with_state(cx, owner, |s| {
                if v >= 0 && (v as usize) < s.primary.len() {
                    s.manager_index = v;
                }
                (s.window, s.manager_index as usize)
            })?;
            krkr_engine::extensions::draw_device_input_manager(cx, window, index)
        }
        #[tjs::getter(name = "stretchType")]
        fn stretch(&self) -> i64 {
            i64::from(self.stretch)
        }
        #[tjs::setter(name = "stretchType")]
        fn set_stretch(&mut self, #[tjs(coerce)] v: i32) {
            self.stretch = v;
        }
        #[tjs::getter(name = "bicubicParam")]
        fn bicubic(&self) -> f64 {
            self.bicubic
        }
        #[tjs::setter(name = "bicubicParam")]
        fn set_bicubic(&mut self, v: f64) {
            self.bicubic = v;
        }
        #[tjs::getter(name = "transState")]
        fn transition(&self) -> f64 {
            self.trans_state
        }
        #[tjs::setter(name = "transState")]
        fn set_transition(&mut self, v: f64) {
            self.trans_state = v;
        }
        #[tjs::getter(name = "maskMode")]
        fn mask(&self) -> i64 {
            i64::from(self.mask)
        }
        #[tjs::setter(name = "maskMode")]
        fn set_mask(&mut self, #[tjs(coerce)] v: i32) {
            self.mask = v;
        }
        #[tjs::method(name = "setScreenRect")]
        fn screen(
            &mut self,
            #[tjs(coerce)] x: i32,
            #[tjs(coerce)] y: i32,
            #[tjs(coerce)] w: i32,
            #[tjs(coerce)] h: i32,
        ) {
            self.screen = [x, y, w, h];
        }
        #[tjs::method(name = "setOffset")]
        fn offset(&mut self, #[tjs(coerce)] x: i32, #[tjs(coerce)] y: i32) {
            self.offset = [x, y];
        }
        #[tjs::invalidate]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let owner = cx.this();
            let window = cx.with_state::<Self, _>(|s, _| Ok(s.window))?;
            krkr_engine::extensions::detach_draw_device(cx, window, owner)?;
            let layers = cx.with_state::<Self, _>(|s, _| {
                s.primary.clear();
                s.manager_images.clear();
                s.window = Value::Void;
                s.composite = None;
                s.previous = None;
                s.generation = s.generation.wrapping_add(1);
                Ok(std::mem::take(&mut s.layers))
            })?;
            for layer in layers {
                if let Ok(id) = crate::exports::object(layer) {
                    let _ = super::super::layer::bindings::with_state(cx, id, |s| {
                        s.device = Value::Void;
                        s.emote = None;
                        s.emote_generation = s.emote_generation.wrapping_add(1);
                    });
                }
            }
            Ok(())
        }
    }
}
#[tjs_bind::function]
fn attach(cx: &mut NativeCx<'_>, window: Value) -> NativeResult<()> {
    let primary = if matches!(window, Value::Void) {
        Vec::new()
    } else {
        krkr_engine::extensions::window_root_layers(cx, window)?
    };
    let index = cx.with_state::<bindings::State, _>(|s, _| {
        s.window = window;
        s.primary = primary;
        s.manager_images.clear();
        s.generation = s.generation.wrapping_add(1);
        Ok(s.manager_index as usize)
    })?;
    krkr_engine::extensions::draw_device_input_manager(cx, window, index)
}
pub(super) fn refresh_roots(cx: &mut NativeCx<'_>, owner: tjs_core::ObjId) -> NativeResult<()> {
    let window = bindings::with_state(cx, owner, |s| s.window)?;
    if matches!(window, Value::Void) {
        return Ok(());
    }
    let primary = krkr_engine::extensions::window_root_layers(cx, window)?;
    bindings::with_state(cx, owner, |s| s.primary = primary)
}
