//! Reference author/license notices: reference-notices.txt.
//! Kirikiri2 win32ole/main.cpp, specifically its absent IDispatch/window paths.
//! Objects and geometry remain usable; OLE dispatch reports failure, never success.
use tjs_bind::RestArgs;
use tjs_core::{NativeCx, NativeError, NativeResult, Value, value};
krkr_engine::native_plugin! {
    pub(crate) Ole {names:["win32ole.dll","win32ole.tpm"],classes:[ole,active],extensions:[],}
}
fn unavailable() -> NativeResult<()> {
    Err(NativeError::Message(
        "OLE/ActiveX dispatch is unavailable on this engine",
    ))
}
#[tjs_bind::class(name = "WIN32OLE")]
mod ole {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, _name: Value) -> NativeResult<Self> {
            let owner = cx.this();
            cx.heap_mut().set_call_missing(owner)?;
            Ok(Self)
        }

        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method]
        fn invoke(&self, _args: RestArgs<'_>) -> NativeResult<()> {
            unavailable()
        }
        #[tjs::method]
        fn get(&self, _args: RestArgs<'_>) -> NativeResult<()> {
            unavailable()
        }
        #[tjs::method]
        fn set(&self, _args: RestArgs<'_>) -> NativeResult<()> {
            unavailable()
        }
        #[tjs::method]
        fn missing(&self, _put: Value, _name: Value, _result: Value) -> bool {
            false
        }
        #[tjs::method(name = "addEvent")]
        fn event(&self, _name: Value, args: RestArgs<'_>) -> NativeResult<()> {
            if let Some(v) = args.first()
                && !matches!(v, Value::Obj(_))
            {
                return Err(NativeError::Type("an event receiver object"));
            }
            Ok(())
        }
        #[tjs::method(name = "getConstant")]
        fn constants(&self, args: RestArgs<'_>) -> NativeResult<()> {
            if let Some(v) = args.first()
                && !matches!(v, Value::Obj(_))
            {
                return Err(NativeError::Type("a constant destination object"));
            }
            Ok(())
        }
    }
}
#[tjs_bind::class(name = "ActiveX")]
mod active {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        left: i32,
        top: i32,
        width: i32,
        height: i32,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                left: 0,
                top: 0,
                width: -1,
                height: -1,
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, _name: Value, args: RestArgs<'_>) -> NativeResult<Self> {
            let mut state = Self::default();
            if args.len() >= 5 {
                state.left = value::to_integer(cx.heap(), args[1])? as i32;
                state.top = value::to_integer(cx.heap(), args[2])? as i32;
                state.width = value::to_integer(cx.heap(), args[3])? as i32;
                state.height = value::to_integer(cx.heap(), args[4])? as i32;
            }
            if let Some(Value::Obj(r)) = args.first()
                && let Some(obj) = r.object
                && !cx
                    .heap()
                    .class_names(obj)?
                    .iter()
                    .any(|n| n == &"Window".encode_utf16().collect::<Vec<_>>())
            {
                return Err(NativeError::Type("a Window object"));
            }
            let owner = cx.this();
            cx.heap_mut().set_call_missing(owner)?;
            Ok(state)
        }

        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method]
        fn invoke(&self, _args: RestArgs<'_>) -> NativeResult<()> {
            unavailable()
        }
        #[tjs::method]
        fn get(&self, _args: RestArgs<'_>) -> NativeResult<()> {
            unavailable()
        }
        #[tjs::method]
        fn set(&self, _args: RestArgs<'_>) -> NativeResult<()> {
            unavailable()
        }
        #[tjs::method]
        fn missing(&self, _put: Value, _name: Value, _result: Value) -> bool {
            false
        }
        #[tjs::method(name = "addEvent")]
        fn event(&self, _name: Value, args: RestArgs<'_>) -> NativeResult<()> {
            if let Some(v) = args.first()
                && !matches!(v, Value::Obj(_))
            {
                return Err(NativeError::Type("an event receiver object"));
            }
            Ok(())
        }
        #[tjs::method(name = "getConstant")]
        fn constants(&self, args: RestArgs<'_>) -> NativeResult<()> {
            if let Some(v) = args.first()
                && !matches!(v, Value::Obj(_))
            {
                return Err(NativeError::Type("a constant destination object"));
            }
            Ok(())
        }

        #[tjs::method(name = "setExternalUI")]
        fn ui(&self) {}
        #[tjs::method(name = "setPos")]
        fn pos(&mut self, left: i64, top: i64) {
            self.left = left as i32;
            self.top = top as i32;
        }
        #[tjs::method(name = "setSize")]
        fn size(&mut self, width: i64, height: i64) {
            self.width = width as i32;
            self.height = height as i32;
        }
        #[tjs::getter(name = "isValidWindow")]
        fn valid(&self) -> bool {
            false
        }
        #[tjs::getter(name = "visible")]
        fn visible(&self) -> bool {
            false
        }
        #[tjs::setter(name = "visible")]
        fn set_visible(&mut self, _visible: bool) {}
        #[tjs::getter(name = "left")]
        fn left(&self) -> i64 {
            self.left.into()
        }
        #[tjs::setter(name = "left")]
        fn set_left(&mut self, value: i64) {
            self.left = value as i32;
        }
        #[tjs::getter(name = "top")]
        fn top(&self) -> i64 {
            self.top.into()
        }
        #[tjs::setter(name = "top")]
        fn set_top(&mut self, value: i64) {
            self.top = value as i32;
        }
        #[tjs::getter(name = "width")]
        fn width(&self) -> i64 {
            self.width.into()
        }
        #[tjs::setter(name = "width")]
        fn set_width(&mut self, value: i64) {
            self.width = value as i32;
        }
        #[tjs::getter(name = "height")]
        fn height(&self) -> i64 {
            self.height.into()
        }
        #[tjs::setter(name = "height")]
        fn set_height(&mut self, value: i64) {
            self.height = value as i32;
        }
    }
}
