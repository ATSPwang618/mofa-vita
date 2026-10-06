use super::*;
use crate::operations::{Operations, Request};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, RestArgs, WaitMode, value};

fn units(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), value)? else {
        unreachable!()
    };
    Ok(name::c_string(cx.heap().string(id)?).to_vec())
}
fn string(cx: &mut NativeCx<'_>, value: Vec<u16>) -> Value {
    Value::Str(cx.heap_mut().alloc_string(value))
}
pub(super) fn service(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let class = cx
        .heap()
        .registered_class("System")
        .expect("installed System");
    cx.heap_mut()
        .with_native_state::<implementation::State, _>(class, |state| {
            state.shared.as_ref().expect("System service").clone()
        })
}
fn exit_code(cx: &NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<i32> {
    Ok(args
        .first()
        .map(|&v| value::to_integer(cx.heap(), v))
        .transpose()?
        .unwrap_or(0) as i32)
}
fn display(cx: &mut NativeCx<'_>) -> NativeResult<krkr_protocol::window::Display> {
    service(cx)?
        .borrow()
        .input
        .as_ref()
        .ok_or(NativeError::Message(
            "display information requires a platform window host",
        ))?
        .display()
        .map_err(NativeError::Detail)
}
struct Returned;
impl Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(value))
    }
}

#[tjs_bind::class(name = "System", static_class = true)]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub shared: Option<Shared>,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            if let Some(shared) = &self.shared {
                shared.borrow().trace(visit);
            }
        }
    }
    impl State {
        /// Portable LEGACY-06 behavior: a missing registry value is void.
        #[tjs::method(name = "readRegValue", class_only = true)]
        fn read_registry(cx: &mut NativeCx<'_>, key: Value) -> NativeResult<()> {
            if cx.result_needed() {
                let _ = units(cx, key)?;
            }
            Ok(())
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn inform(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            super::super::files::inform(cx, args)
        }
        #[tjs::method(name = "toActualColor", class_only = true)]
        fn actual_color(cx: &mut NativeCx<'_>, color: Value) -> NativeResult<i64> {
            Ok(crate::color::actual(value::to_integer(cx.heap(), color)? as u32).into())
        }
        #[tjs::getter(name = "graphicCacheLimit", class_only = true)]
        fn graphic_cache_limit(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(service(cx)?.borrow().operations.borrow().images.limit() as i64)
        }
        #[tjs::setter(name = "graphicCacheLimit", class_only = true)]
        fn set_graphic_cache_limit(cx: &mut NativeCx<'_>, bytes: Value) -> NativeResult<()> {
            let bytes = value::to_integer(cx.heap(), bytes)? as i32;
            service(cx)?
                .borrow()
                .operations
                .borrow()
                .images
                .set_limit(bytes);
            Ok(())
        }
        #[tjs::method(name = "clearGraphicCache", class_only = true)]
        fn clear_graphic_cache(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            service(cx)?.borrow().operations.borrow().images.clear();
            Ok(())
        }
        #[tjs::method(name = "touchImages", class_only = true, resumable = true)]
        fn touch_images(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            crate::layer::preload::start(cx, args)
        }
        #[tjs::getter(name = "screenWidth", class_only = true)]
        fn screen_width(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(display(cx)?.width.into())
        }
        #[tjs::getter(name = "screenHeight", class_only = true)]
        fn screen_height(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(display(cx)?.height.into())
        }
        #[tjs::getter(name = "desktopLeft", class_only = true)]
        fn desktop_left(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(display(cx)?.desktop_left.into())
        }
        #[tjs::getter(name = "desktopTop", class_only = true)]
        fn desktop_top(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(display(cx)?.desktop_top.into())
        }
        #[tjs::getter(name = "desktopWidth", class_only = true)]
        fn desktop_width(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(display(cx)?.desktop_width.into())
        }
        #[tjs::getter(name = "desktopHeight", class_only = true)]
        fn desktop_height(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(display(cx)?.desktop_height.into())
        }
        #[tjs::method(name = "getKeyState")]
        fn get_key_state(
            cx: &mut NativeCx<'_>,
            key: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<bool> {
            let key = value::to_integer(cx.heap(), key)? as u32;
            let current = args
                .first()
                .map(|&value| value::to_integer(cx.heap(), value))
                .transpose()?
                .unwrap_or(1)
                != 0;
            let shared = service(cx)?;
            let world = shared.borrow();
            let input = world.input.as_ref().ok_or(NativeError::Message(
                "getKeyState requires a platform input host",
            ))?;
            Ok(input.key_state(key, current))
        }
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method(name = "getTickCount", class_only = true)]
        fn tick(cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            Ok(service(cx)?.borrow().clock.now().as_millis() as i64)
        }
        #[tjs::method(name = "createUUID", class_only = true)]
        fn uuid() -> String {
            uuid::Uuid::new_v4().to_string()
        }
        #[tjs::method(name = "createAppLock", class_only = true)]
        fn app_lock(cx: &mut NativeCx<'_>, name: Value) -> NativeResult<bool> {
            let name = units(cx, name)?;
            service(cx)?
                .borrow_mut()
                .config
                .host
                .as_mut()
                .ok_or(NativeError::Message(
                    "createAppLock requires a platform host",
                ))?
                .create_app_lock(&name)
                .map_err(NativeError::Detail)
        }
        #[tjs::method(name = "getArgument", class_only = true)]
        fn get_argument(cx: &mut NativeCx<'_>, key: Value) -> NativeResult<Value> {
            let key = units(cx, key)?;
            Ok(service(cx)?
                .borrow()
                .config
                .arguments
                .get(&key)
                .cloned()
                .map(|text| string(cx, text))
                .unwrap_or(Value::Void))
        }
        #[tjs::method(name = "setArgument", class_only = true)]
        fn set_argument(cx: &mut NativeCx<'_>, key: Value, val: Value) -> NativeResult<()> {
            service(cx)?
                .borrow_mut()
                .config
                .arguments
                .insert(units(cx, key)?, units(cx, val)?);
            Ok(())
        }
        #[tjs::method(name = "addContinuousHandler", class_only = true)]
        fn add(cx: &mut NativeCx<'_>, function: Value) -> NativeResult<()> {
            service(cx)?.borrow_mut().add(function)
        }
        #[tjs::method(name = "removeContinuousHandler", class_only = true)]
        fn remove(cx: &mut NativeCx<'_>, function: Value) -> NativeResult<()> {
            if !matches!(function, Value::Obj(_)) {
                return Err(NativeError::Type("a function object"));
            }
            service(cx)?.borrow_mut().remove(function);
            Ok(())
        }
        #[tjs::method(class_only = true)]
        fn terminate(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            service(cx)?.borrow_mut().exit = Some(exit_code(cx, args)?);
            Ok(())
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn exit(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let code = exit_code(cx, args)?;
            Operations::wait(
                &service(cx)?.borrow().operations,
                Request::Exit(code),
                WaitMode::Internal,
                Box::new(Returned),
            )
        }
        #[tjs::method(name = "doCompact", class_only = true, resumable = true)]
        fn compact(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let level = args
                .first()
                .filter(|v| !matches!(v, Value::Void))
                .map(|&v| value::to_integer(cx.heap(), v))
                .transpose()?
                .unwrap_or(100) as i32;
            Operations::wait(
                &service(cx)?.borrow().operations,
                Request::Compact(level),
                WaitMode::Internal,
                Box::new(Returned),
            )
        }
        /// Modern extension: a monotonic wait that permits nested engine events.
        #[tjs::method(class_only = true, resumable = true)]
        fn wait(cx: &mut NativeCx<'_>, milliseconds: f64) -> NativeResult<NativeStep> {
            let delay = Duration::try_from_secs_f64(milliseconds / 1000.0).map_err(|_| {
                NativeError::Message("wait duration must be finite and nonnegative")
            })?;
            let shared = service(cx)?;
            let system = shared.borrow();
            let deadline = system
                .clock
                .now()
                .checked_add(delay)
                .ok_or(NativeError::Message("wait deadline overflow"))?;
            Operations::wait(
                &system.operations,
                Request::Delay(deadline),
                WaitMode::Event,
                Box::new(Returned),
            )
        }
        #[tjs::getter(name = "eventDisabled", class_only = true)]
        fn disabled(&self) -> bool {
            self.shared.as_ref().unwrap().borrow().event_disabled
        }
        #[tjs::setter(name = "eventDisabled", class_only = true)]
        fn set_disabled(&mut self, disabled: bool) {
            self.shared.as_ref().unwrap().borrow_mut().event_disabled = disabled;
        }
        #[tjs::getter(name = "exitOnWindowClose", class_only = true)]
        fn exit_on_window_close(&self) -> bool {
            self.shared.as_ref().unwrap().borrow().exit_on_window_close
        }
        #[tjs::setter(name = "exitOnWindowClose", class_only = true)]
        fn set_exit_on_window_close(
            &mut self,
            cx: &NativeCx<'_>,
            value: Value,
        ) -> NativeResult<()> {
            let enabled = value::to_integer(cx.heap(), value)? as i32 != 0;
            self.shared
                .as_ref()
                .unwrap()
                .borrow_mut()
                .exit_on_window_close = enabled;
            Ok(())
        }
        #[tjs::getter(name = "exitOnNoWindowStartup", class_only = true)]
        fn exit_on_no_window_startup(&self) -> bool {
            self.shared
                .as_ref()
                .unwrap()
                .borrow()
                .exit_on_no_window_startup
        }
        #[tjs::setter(name = "exitOnNoWindowStartup", class_only = true)]
        fn set_exit_on_no_window_startup(
            &mut self,
            cx: &NativeCx<'_>,
            value: Value,
        ) -> NativeResult<()> {
            let enabled = value::to_integer(cx.heap(), value)? as i32 != 0;
            self.shared
                .as_ref()
                .unwrap()
                .borrow_mut()
                .exit_on_no_window_startup = enabled;
            Ok(())
        }
        #[tjs::getter(name = "versionString", class_only = true)]
        fn version(&self) -> String {
            concat!("krkr-rs ", env!("CARGO_PKG_VERSION")).into()
        }
        #[tjs::getter(name = "versionInformation", class_only = true)]
        fn information(&self) -> String {
            concat!(
                "krkr-rs ",
                env!("CARGO_PKG_VERSION"),
                " / Rust TJS2 runtime"
            )
            .into()
        }
        #[tjs::getter(name = "platformName", class_only = true)]
        fn platform(&self) -> String {
            "krkr-rs".into()
        }
        #[tjs::getter(name = "osName", class_only = true)]
        fn os(&self) -> String {
            std::env::consts::OS.into()
        }
        #[tjs::getter(name = "processorNum", class_only = true)]
        fn processors(&self) -> NativeResult<i64> {
            std::thread::available_parallelism()
                .map(|n| n.get() as i64)
                .map_err(|e| NativeError::Detail(e.to_string()))
        }
        #[tjs::getter(name = "exeBits", class_only = true)]
        fn bits(&self) -> i64 {
            usize::BITS as i64
        }
        #[tjs::getter(name = "exeName", class_only = true)]
        fn exe_name(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .config
                    .exe_name
                    .clone(),
            )
        }
        #[tjs::getter(name = "exePath", class_only = true)]
        fn exe_path(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .config
                    .exe_path
                    .clone(),
            )
        }
        #[tjs::getter(name = "dataPath", class_only = true)]
        fn data_path(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .config
                    .data_path
                    .clone(),
            )
        }
        #[tjs::getter(class_only = true)]
        fn title(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared.as_ref().unwrap().borrow().config.title.clone(),
            )
        }
        #[tjs::getter(name = "personalPath", class_only = true)]
        fn personal_path(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .config
                    .personal_path
                    .clone(),
            )
        }
        #[tjs::getter(name = "appDataPath", class_only = true)]
        fn app_data_path(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .config
                    .app_data_path
                    .clone(),
            )
        }
        #[tjs::getter(name = "savedGamesPath", class_only = true)]
        fn saved_games_path(&self, cx: &mut NativeCx<'_>) -> Value {
            string(
                cx,
                self.shared
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .config
                    .saved_games_path
                    .clone(),
            )
        }
        #[tjs::setter(name = "title", class_only = true)]
        fn set_title(&mut self, cx: &mut NativeCx<'_>, title: Value) -> NativeResult<()> {
            self.shared.as_ref().unwrap().borrow_mut().config.title = units(cx, title)?;
            Ok(())
        }
    }
}
pub(super) fn install(heap: &mut Heap, shared: Shared) -> NativeResult<ObjId> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |state| state.shared = Some(shared))?;
    Ok(class)
}
