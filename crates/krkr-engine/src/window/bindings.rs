use super::tasks::{Change, Closing, Returned, object, request};
use super::*;
use tjs_core::{NativeCx, NativeStep, ObjRef, value};
fn dimension(value: i64) -> u32 {
    (value as i32).max(1) as u32
}
pub(crate) use implementation::State;
pub(crate) fn register_extended_events(
    heap: &mut Heap,
    object: ObjId,
    has_move: bool,
) -> NativeResult<()> {
    heap.with_native_state::<State, _>(object, |state| {
        let lease = state.lease.as_ref().ok_or(NativeError::This)?;
        let mut world = lease.shared.borrow_mut();
        let record = world.record_mut(lease.id)?;
        record.extended_events = true;
        record.move_event = has_move;
        Ok(())
    })?
}
pub(crate) fn id(heap: &mut Heap, object: ObjId) -> NativeResult<WindowId> {
    heap.with_native_state::<State, _>(object, |s| s.lease.as_ref().map(|lease| lease.id))?
        .ok_or(NativeError::This)
}
pub(crate) fn attach_menu(heap: &mut Heap, window: ObjId, menu: ObjId) -> NativeResult<()> {
    heap.with_native_state::<State, _>(window, |s| {
        let lease = s.lease()?;
        if !lease.shared.borrow().is_live(lease.id) {
            return Err(NativeError::Message("window is closing"));
        }
        super::lifetime::associate(s, object(menu), true)
    })?
}
pub(crate) fn menu_events_allowed(heap: &mut Heap, window: ObjId) -> NativeResult<bool> {
    if !heap.is_valid(window)? {
        return Ok(false);
    }
    heap.with_native_state::<State, _>(window, |s| {
        let lease = s.lease()?;
        let world = lease.shared.borrow();
        Ok(world.is_live(lease.id) && world.record(lease.id)?.visible)
    })?
}
#[tjs_bind::class(name = "Window")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        pub lease: Option<Lease>,
        // The native instance owns these even before super.Window creates a host window.
        pub(in crate::window) associated: Vec<Value>,
        pub(in crate::window) invalidating: std::rc::Weak<()>,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.associated.trace(visit);
            if let Some(shared) = &self.service {
                shared.borrow().trace(visit);
            }
            if let Some(lease) = &self.lease
                && let Ok(record) = lease.shared.borrow().record(lease.id)
            {
                record.menu.trace(visit);
                lease.shared.borrow().trace_input(lease.id, visit);
            }
        }
    }
    impl State {
        #[tjs::getter(name = "drawDevice")]
        fn draw_device(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            super::super::draw_device::get(cx)
        }
        #[tjs::setter(name = "drawDevice", resumable = true)]
        fn set_draw_device(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            super::super::draw_device::set(cx, input)
        }
        pub(in crate::window) fn lease(&self) -> NativeResult<&Lease> {
            self.lease.as_ref().ok_or(NativeError::This)
        }
        fn command(&self, command: Command, change: Change) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            request(&lease.shared, lease.id, command, change, None)
        }
        fn action(
            &self,
            cx: &mut NativeCx<'_>,
            index: usize,
            fields: &[usize],
            args: &[Value],
        ) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let (event_type, action, keys) = {
                let world = lease.shared.borrow();
                (world.names[index], world.names[15], world.fields)
            };
            super::super::callbacks::action(cx, event_type, action, &keys, fields, args)
        }
        #[tjs::constructor(resumable = true)]
        fn create(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let class = cx
                .heap()
                .registered_class("Window")
                .expect("installed Window");
            let shared = cx
                .heap_mut()
                .with_native_state::<State, _>(class, |s| s.service.clone())?
                .expect("Window world");
            let lease = Windows::create(&shared, cx.this())?;
            let id = lease.id;
            let alive = Arc::downgrade(&shared.borrow().record(id)?.alive);
            request(
                &shared,
                id,
                Command::Create {
                    alive,
                    caption: String::new(),
                },
                Change::None,
                Some(lease),
            )
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::getter(name = "borderStyle")]
        fn border_style(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.border_style as i64)
        }
        #[tjs::setter(name = "borderStyle", resumable = true)]
        fn set_border_style(
            &self,
            cx: &mut NativeCx<'_>,
            value: Value,
        ) -> NativeResult<NativeStep> {
            let style = krkr_protocol::window::BorderStyle::try_from(value::to_integer(
                cx.heap(),
                value,
            )? as i32)
            .map_err(NativeError::Message)?;
            self.command(Command::BorderStyle(style), Change::BorderStyle(style))
        }
        #[tjs::method(name = "beginMove", resumable = true)]
        fn begin_move(&self) -> NativeResult<NativeStep> {
            self.command(Command::BeginMove, Change::None)
        }
        #[tjs::getter(name = "focusedLayer")]
        fn focused_layer(&self) -> NativeResult<Value> {
            let lease = self.lease()?;
            let layers = lease
                .shared
                .borrow()
                .layers
                .upgrade()
                .expect("installed Layer");
            Ok(crate::layer::focused_layer(&layers, lease.id))
        }
        #[tjs::setter(name = "focusedLayer", resumable = true)]
        fn set_focused_layer(
            &self,
            cx: &mut NativeCx<'_>,
            value: Value,
        ) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let layers = lease
                .shared
                .borrow()
                .layers
                .upgrade()
                .expect("installed Layer");
            crate::layer::set_focused_layer(layers, lease.id, cx.heap_mut(), value)
        }
        #[tjs::method(name = "setZoom", resumable = true)]
        fn set_zoom(
            &self,
            cx: &mut NativeCx<'_>,
            numer: Value,
            denom: Value,
        ) -> NativeResult<NativeStep> {
            let viewport = self
                .viewport()?
                .zoom(
                    value::to_integer(cx.heap(), numer)? as i32,
                    value::to_integer(cx.heap(), denom)? as i32,
                )
                .map_err(NativeError::Message)?;
            self.change_viewport(cx, viewport)
        }
        #[tjs::getter(name = "zoomNumer")]
        fn zoom_numer(&self) -> NativeResult<i64> {
            Ok(self.viewport()?.numer().into())
        }
        #[tjs::getter(name = "zoomDenom")]
        fn zoom_denom(&self) -> NativeResult<i64> {
            Ok(self.viewport()?.denom().into())
        }
        #[tjs::setter(name = "zoomNumer", resumable = true)]
        fn set_zoom_numer(&self, cx: &mut NativeCx<'_>, numer: Value) -> NativeResult<NativeStep> {
            self.set_zoom(cx, numer, Value::Int(self.zoom_denom()?))
        }
        #[tjs::setter(name = "zoomDenom", resumable = true)]
        fn set_zoom_denom(&self, cx: &mut NativeCx<'_>, denom: Value) -> NativeResult<NativeStep> {
            self.set_zoom(cx, Value::Int(self.zoom_numer()?), denom)
        }
        #[tjs::method(name = "setLayerPos", resumable = true)]
        fn set_layer_pos(
            &self,
            cx: &mut NativeCx<'_>,
            left: Value,
            top: Value,
        ) -> NativeResult<NativeStep> {
            let mut viewport = self.viewport()?;
            viewport.left = value::to_integer(cx.heap(), left)? as i32;
            viewport.top = value::to_integer(cx.heap(), top)? as i32;
            self.change_viewport(cx, viewport)
        }
        #[tjs::getter(name = "layerLeft")]
        fn layer_left(&self) -> NativeResult<i64> {
            Ok(self.viewport()?.left.into())
        }
        #[tjs::getter(name = "layerTop")]
        fn layer_top(&self) -> NativeResult<i64> {
            Ok(self.viewport()?.top.into())
        }
        #[tjs::setter(name = "layerLeft", resumable = true)]
        fn set_layer_left(&self, cx: &mut NativeCx<'_>, left: Value) -> NativeResult<NativeStep> {
            self.set_layer_pos(cx, left, Value::Int(self.layer_top()?))
        }
        #[tjs::setter(name = "layerTop", resumable = true)]
        fn set_layer_top(&self, cx: &mut NativeCx<'_>, top: Value) -> NativeResult<NativeStep> {
            self.set_layer_pos(cx, Value::Int(self.layer_left()?), top)
        }
        #[tjs::getter(name = "mouseCursorState")]
        fn mouse_cursor_state(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.cursor_state.into())
        }
        #[tjs::setter(name = "mouseCursorState", resumable = true)]
        fn set_mouse_cursor_state(
            &self,
            cx: &mut NativeCx<'_>,
            state: Value,
        ) -> NativeResult<NativeStep> {
            let state = value::to_integer(cx.heap(), state)? as i32;
            self.command(Command::CursorState(state), Change::CursorState(state))
        }
        #[tjs::getter(name = "hintDelay")]
        fn hint_delay(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.hint.delay.into())
        }
        #[tjs::setter(name = "hintDelay")]
        fn set_hint_delay(&self, cx: &mut NativeCx<'_>, delay: Value) -> NativeResult<()> {
            let delay = value::to_integer(cx.heap(), delay)? as i32;
            let lease = self.lease()?;
            lease.shared.borrow_mut().record_mut(lease.id)?.hint.delay = delay;
            Ok(())
        }
        #[tjs::getter(name = "imeMode")]
        fn ime_mode(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.default_ime.into())
        }
        #[tjs::setter(name = "imeMode", resumable = true)]
        fn set_ime_mode(&self, cx: &mut NativeCx<'_>, mode: Value) -> NativeResult<NativeStep> {
            let mode = value::to_integer(cx.heap(), mode)? as i32;
            let lease = self.lease()?;
            let layers = {
                let mut world = lease.shared.borrow_mut();
                world.record_mut(lease.id)?.default_ime = mode;
                world.layers.upgrade().expect("installed Layer")
            };
            crate::layer::sync_input(&layers, lease.id, cx, Value::Void, Box::new(Returned))
        }
        #[tjs::method(name = "postInputEvent", resumable = true)]
        fn post_input_event(
            &self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            super::super::input::post(self.lease()?, cx, name, args.first().copied())
        }
        #[tjs::invalidate(resumable = true)]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let owner = cx.this();
            let has_lease = cx
                .heap_mut()
                .with_native_state::<Self, _>(owner, |s| s.lease.is_some())?;
            if !has_lease {
                return super::super::lifetime::invalidate(cx, owner);
            }
            let step = super::super::draw_device::set(cx, Value::Void)?;
            Ok(tjs_bind::flow::then(
                step,
                tjs_bind::flow::callback(owner, |owner, cx, _| {
                    super::super::lifetime::invalidate(cx, owner)
                }),
            ))
        }
        #[tjs::method]
        fn add(&mut self, value: Value) -> NativeResult<()> {
            super::super::lifetime::associate(self, value, true)
        }
        #[tjs::method]
        fn remove(&mut self, value: Value) -> NativeResult<()> {
            super::super::lifetime::associate(self, value, false)
        }
        #[tjs::getter(name = "primaryLayer")]
        fn primary_layer(&self) -> NativeResult<Value> {
            let lease = self.lease()?;
            let layers = lease
                .shared
                .borrow()
                .layers
                .upgrade()
                .expect("installed Layer");
            layers.borrow().primary(lease.id)
        }
        /// Deprecated script compatibility; no native menu is created.
        #[tjs::getter(name = "menu")]
        fn menu(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let lease = self.lease()?;
            if let Some(menu) = lease.shared.borrow().record(lease.id)?.menu {
                return Ok(object(menu));
            }
            if !lease.shared.borrow().is_live(lease.id) {
                return Err(NativeError::Message("window is closing"));
            }
            let owner = cx.this();
            let menu = crate::menu::root(cx.heap_mut(), owner)?;
            lease.shared.borrow_mut().record_mut(lease.id)?.menu = Some(menu);
            Ok(object(menu))
        }
        #[tjs::getter(name = "mainWindow")]
        fn main_window(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let class = cx
                .heap()
                .registered_class("Window")
                .expect("installed Window");
            let shared = cx
                .heap_mut()
                .with_native_state::<State, _>(class, |s| s.service.clone())?
                .expect("Window world");
            let world = shared.borrow();
            Ok(world
                .main
                .and_then(|id| world.records.get(id))
                .filter(|record| record.invalidating.strong_count() == 0)
                .map(|r| object(r.owner))
                .unwrap_or(Value::Obj(ObjRef {
                    object: None,
                    this: None,
                })))
        }
        #[tjs::getter]
        fn caption(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let lease = self.lease()?;
            let text = lease.shared.borrow().record(lease.id)?.caption.clone();
            Ok(Value::Str(cx.heap_mut().alloc_string(text)))
        }
        #[tjs::setter(name = "caption", resumable = true)]
        fn set_caption(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            let Value::Str(id) = value::to_string(cx.heap_mut(), input)? else {
                unreachable!()
            };
            let text = tjs_core::string::c_string(cx.heap().string(id)?).to_vec();
            self.command(
                Command::Caption(String::from_utf16_lossy(&text)),
                Change::Caption(text),
            )
        }

        #[tjs::getter(name = "visible")]
        fn get_visible(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.visible)
        }

        #[tjs::getter(name = "stayOnTop")]
        fn get_stay_on_top(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.stay_on_top)
        }

        #[tjs::getter(name = "fullScreen")]
        fn get_full_screen(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease
                .shared
                .borrow()
                .record(lease.id)?
                .full_screen
                .is_some())
        }

        #[tjs::getter(name = "width")]
        fn get_width(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.geometry.width as i64)
        }

        #[tjs::getter(name = "height")]
        fn get_height(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.geometry.height as i64)
        }

        #[tjs::getter(name = "innerWidth")]
        fn get_inner_width(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.geometry.inner_width as i64)
        }

        #[tjs::getter(name = "innerHeight")]
        fn get_inner_height(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease
                .shared
                .borrow()
                .record(lease.id)?
                .geometry
                .inner_height as i64)
        }

        #[tjs::getter(name = "left")]
        fn get_left(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.geometry.left as i64)
        }

        #[tjs::getter(name = "top")]
        fn get_top(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.geometry.top as i64)
        }

        #[tjs::getter(name = "minWidth")]
        fn get_min_width(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.min_size.0 as i64)
        }

        #[tjs::getter(name = "minHeight")]
        fn get_min_height(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.min_size.1 as i64)
        }

        #[tjs::getter(name = "maxWidth")]
        fn get_max_width(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.max_size.0 as i64)
        }

        #[tjs::getter(name = "maxHeight")]
        fn get_max_height(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().record(lease.id)?.max_size.1 as i64)
        }

        #[tjs::setter(name = "visible", resumable = true)]
        fn set_visible(&self, value: bool) -> NativeResult<NativeStep> {
            self.command(Command::Visible(value), Change::Visible(value))
        }

        #[tjs::setter(name = "stayOnTop", resumable = true)]
        fn set_stay_on_top(&self, value: bool) -> NativeResult<NativeStep> {
            self.command(Command::StayOnTop(value), Change::StayOnTop(value))
        }

        #[tjs::setter(name = "fullScreen", resumable = true)]
        fn set_full_screen(&self, value: bool) -> NativeResult<NativeStep> {
            self.command(Command::FullScreen(value), Change::FullScreen(value))
        }

        #[tjs::setter(name = "width", resumable = true)]
        fn set_width(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let other = lease.shared.borrow().record(lease.id)?.geometry.height;
            self.command(
                Command::Size {
                    width: dimension(value),
                    height: other,
                    inner: false,
                },
                Change::None,
            )
        }

        #[tjs::setter(name = "height", resumable = true)]
        fn set_height(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let other = lease.shared.borrow().record(lease.id)?.geometry.width;
            self.command(
                Command::Size {
                    width: other,
                    height: dimension(value),
                    inner: false,
                },
                Change::None,
            )
        }

        #[tjs::setter(name = "innerWidth", resumable = true)]
        fn set_inner_width(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let other = lease
                .shared
                .borrow()
                .record(lease.id)?
                .geometry
                .inner_height;
            self.command(
                Command::Size {
                    width: dimension(value),
                    height: other,
                    inner: true,
                },
                Change::None,
            )
        }

        #[tjs::setter(name = "innerHeight", resumable = true)]
        fn set_inner_height(
            &self,
            cx: &mut NativeCx<'_>,
            value: Value,
        ) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let other = lease.shared.borrow().record(lease.id)?.geometry.inner_width;
            self.command(
                Command::Size {
                    width: other,
                    height: dimension(value),
                    inner: true,
                },
                Change::None,
            )
        }

        #[tjs::setter(name = "minWidth", resumable = true)]
        fn set_min_width(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let mut size = lease.shared.borrow().record(lease.id)?.min_size;
            size.0 = (value as i32).max(0) as u32;
            self.command(
                Command::MinSize(size.0, size.1),
                Change::MinSize(size.0, size.1),
            )
        }

        #[tjs::setter(name = "minHeight", resumable = true)]
        fn set_min_height(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let mut size = lease.shared.borrow().record(lease.id)?.min_size;
            size.1 = (value as i32).max(0) as u32;
            self.command(
                Command::MinSize(size.0, size.1),
                Change::MinSize(size.0, size.1),
            )
        }

        #[tjs::setter(name = "maxWidth", resumable = true)]
        fn set_max_width(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let mut size = lease.shared.borrow().record(lease.id)?.max_size;
            size.0 = (value as i32).max(0) as u32;
            self.command(
                Command::MaxSize(size.0, size.1),
                Change::MaxSize(size.0, size.1),
            )
        }

        #[tjs::setter(name = "maxHeight", resumable = true)]
        fn set_max_height(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            let mut size = lease.shared.borrow().record(lease.id)?.max_size;
            size.1 = (value as i32).max(0) as u32;
            self.command(
                Command::MaxSize(size.0, size.1),
                Change::MaxSize(size.0, size.1),
            )
        }
        #[tjs::setter(name = "left", resumable = true)]
        fn set_left(&self, cx: &mut NativeCx<'_>, left: Value) -> NativeResult<NativeStep> {
            let left = value::to_integer(cx.heap(), left)?;
            let lease = self.lease()?;
            let top = lease.shared.borrow().record(lease.id)?.geometry.top;
            self.command(Command::Position(left as i32, top), Change::None)
        }
        #[tjs::setter(name = "top", resumable = true)]
        fn set_top(&self, cx: &mut NativeCx<'_>, top: Value) -> NativeResult<NativeStep> {
            let top = value::to_integer(cx.heap(), top)?;
            let lease = self.lease()?;
            let left = lease.shared.borrow().record(lease.id)?.geometry.left;
            self.command(Command::Position(left, top as i32), Change::None)
        }
        #[tjs::method(name = "setSize", resumable = true)]
        fn set_size(
            &self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<NativeStep> {
            let width = value::to_integer(cx.heap(), width)?;
            let height = value::to_integer(cx.heap(), height)?;
            self.command(
                Command::Size {
                    width: dimension(width),
                    height: dimension(height),
                    inner: false,
                },
                Change::None,
            )
        }
        #[tjs::method(name = "setInnerSize", resumable = true)]
        fn set_inner_size(
            &self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<NativeStep> {
            let width = value::to_integer(cx.heap(), width)?;
            let height = value::to_integer(cx.heap(), height)?;
            self.command(
                Command::Size {
                    width: dimension(width),
                    height: dimension(height),
                    inner: true,
                },
                Change::None,
            )
        }
        #[tjs::method(name = "setPos", resumable = true)]
        fn set_pos(
            &self,
            cx: &mut NativeCx<'_>,
            left: Value,
            top: Value,
        ) -> NativeResult<NativeStep> {
            let left = value::to_integer(cx.heap(), left)?;
            let top = value::to_integer(cx.heap(), top)?;
            self.command(Command::Position(left as i32, top as i32), Change::None)
        }
        #[tjs::method(name = "setMinSize", resumable = true)]
        fn set_min_size(
            &self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<NativeStep> {
            let width = value::to_integer(cx.heap(), width)?;
            let height = value::to_integer(cx.heap(), height)?;
            let (w, h) = ((width as i32).max(0) as u32, (height as i32).max(0) as u32);
            self.command(Command::MinSize(w, h), Change::MinSize(w, h))
        }
        #[tjs::method(name = "setMaxSize", resumable = true)]
        fn set_max_size(
            &self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<NativeStep> {
            let width = value::to_integer(cx.heap(), width)?;
            let height = value::to_integer(cx.heap(), height)?;
            let (w, h) = ((width as i32).max(0) as u32, (height as i32).max(0) as u32);
            self.command(Command::MaxSize(w, h), Change::MaxSize(w, h))
        }
        #[tjs::method(name = "bringToFront", resumable = true)]
        fn bring_to_front(&self) -> NativeResult<NativeStep> {
            self.command(Command::Focus, Change::None)
        }
        #[tjs::method(resumable = true)]
        fn update(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            // Both reference update types expose the whole window. Preserve
            // its optional argument conversion without inventing extra modes.
            if let Some(&kind) = args.first().filter(|v| !matches!(v, Value::Void)) {
                let _ = value::to_integer(cx.heap(), kind)?;
            }
            let lease = self.lease()?;
            let layers = {
                let windows = lease.shared.borrow();
                windows.record(lease.id)?;
                windows.layers.upgrade().expect("installed Layer")
            };
            Ok(crate::layer::update::window(layers, lease.id, cx.this()))
        }
        #[tjs::method(name = "hideMouseCursor", resumable = true)]
        fn hide_mouse(&self) -> NativeResult<NativeStep> {
            self.command(Command::HideCursor, Change::CursorState(1))
        }
        #[tjs::method(name = "showModal", resumable = true)]
        fn show_modal(&self) -> NativeResult<NativeStep> {
            super::super::modal::show(self.lease()?)
        }
        #[tjs::method(resumable = true)]
        fn close(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let allowed = Rc::new(Cell::new(true));
            {
                let mut world = lease.shared.borrow_mut();
                let record = world.record_mut(lease.id)?;
                if record.user_closing || record.closing.is_some() {
                    return Ok(NativeStep::Return(Value::Void));
                }
                record.closing = Some(allowed.clone());
            }
            Ok(NativeStep::CallMember {
                object: object(cx.this()),
                key: lease.shared.borrow().names[3],
                arguments: vec![Value::Int(1)],
                continuation: Box::new(Closing {
                    shared: lease.shared.clone(),
                    id: lease.id,
                    owner: cx.this(),
                    allowed,
                }),
            })
        }

        #[tjs::method(name = "onResize", resumable = true)]
        fn on_resize(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 0, &[], args)
        }

        #[tjs::method(name = "onActivate", resumable = true)]
        fn on_activate(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 1, &[], args)
        }

        #[tjs::method(name = "onDeactivate", resumable = true)]
        fn on_deactivate(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 2, &[], args)
        }

        #[tjs::method(name = "onMouseEnter", resumable = true)]
        fn on_mouse_enter(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 4, &[], args)
        }

        #[tjs::method(name = "onMouseLeave", resumable = true)]
        fn on_mouse_leave(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 5, &[], args)
        }

        #[tjs::method(name = "onMouseMove", resumable = true)]
        fn on_mouse_move(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 6, &[2, 3, 5], args)
        }

        #[tjs::method(name = "onMouseDown", resumable = true)]
        fn on_mouse_down(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 7, &[2, 3, 4, 5], args)
        }

        #[tjs::method(name = "onMouseUp", resumable = true)]
        fn on_mouse_up(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 8, &[2, 3, 4, 5], args)
        }

        #[tjs::method(name = "onClick", resumable = true)]
        fn on_click(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 9, &[2, 3], args)
        }

        #[tjs::method(name = "onDoubleClick", resumable = true)]
        fn on_double_click(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 10, &[2, 3], args)
        }

        #[tjs::method(name = "onMouseWheel", resumable = true)]
        fn on_mouse_wheel(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 11, &[5, 7, 2, 3], args)
        }

        #[tjs::method(name = "onKeyDown", resumable = true)]
        fn on_key_down(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 12, &[6, 5], args)
        }

        #[tjs::method(name = "onKeyUp", resumable = true)]
        fn on_key_up(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 13, &[6, 5], args)
        }

        #[tjs::method(name = "onKeyPress", resumable = true)]
        fn on_key_press(
            &self,
            cx: &mut NativeCx<'_>,
            args: tjs_core::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.action(cx, 14, &[6], args)
        }
        #[tjs::method(name = "onCloseQuery", resumable = true)]
        fn close_query(&self, cx: &mut NativeCx<'_>, allowed: bool) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let main = {
                let mut world = lease.shared.borrow_mut();
                let record = world.record_mut(lease.id)?;
                if let Some(value) = &record.closing {
                    value.set(allowed);
                    return Ok(NativeStep::Return(Value::Void));
                }
                record.user_closing = false;
                world.main == Some(lease.id)
            };
            if !allowed {
                return Ok(NativeStep::Return(Value::Void));
            }
            if lease.shared.borrow_mut().finish_modal(lease.id) {
                return Ok(NativeStep::Return(Value::Void));
            }
            if main {
                Ok(NativeStep::Invalidate {
                    object: object(cx.this()),
                    continuation: Box::new(Returned),
                })
            } else {
                self.command(Command::Visible(false), Change::Visible(false))
            }
        }
    }
}
pub(super) fn install(heap: &mut Heap, shared: Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    super::draw_device::install(heap, class)?;
    // Deprecated chrome options are script state, with no native resource.
    // Ordinary inherited defaults also allow assignment before super.Window,
    // as Z scripts do when supplying these removed properties themselves.
    for (name, value) in [("innerSunken", 0), ("showScrollBars", 1)] {
        let key = heap.intern_str(name);
        heap.set_member(class, key, Value::Int(value))?;
    }
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |s| s.service = Some(shared))?;
    Ok(())
}
