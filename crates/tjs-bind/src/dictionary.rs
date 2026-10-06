use crate::{NativeCx, NativeResult, NativeStep, RestArgs, Value};

#[crate::class(name = "Dictionary", storage = "dictionary")]
/// TJS Dictionary：动态名字和值。
mod implementation {
    use super::*;

    #[derive(Default, crate::Trace)]
    pub struct State;

    impl State {
        #[tjs::constructor(class_only = true)]
        fn new() -> Self {
            Self
        }

        #[tjs::method(class_only = true, resumable = true)]
        fn assign(
            &mut self,
            cx: &mut NativeCx<'_>,
            source: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let clear = match args.first() {
                None | Some(Value::Void) => true,
                Some(&value) => tjs_core::value::to_integer(cx.heap(), value)? as i32 != 0,
            };
            crate::containers::assign(cx, source, clear, false)
        }
        #[tjs::method(name = "assignStruct", class_only = true, resumable = true)]
        fn assign_struct(
            &mut self,
            cx: &mut NativeCx<'_>,
            source: Value,
        ) -> NativeResult<NativeStep> {
            crate::containers::assign(cx, source, true, true)
        }
        #[tjs::method(name = "loadStruct", class_only = true, resumable = true)]
        fn load_struct(
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::container_io::load_structure(cx, name, args, true)
        }
        #[tjs::method(name = "saveStruct", class_only = true, resumable = true)]
        fn save_struct(
            &self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            crate::container_io::save_structure(cx, name, args)
        }
        // These are successful no-ops in the reference implementation too.
        #[tjs::method(class_only = true)]
        fn load(&self) {}
        #[tjs::method(class_only = true)]
        fn save(&self) {}

        /// 清空动态字段。
        #[tjs::method(class_only = true)]
        fn clear(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let this = cx.this();
            Ok(cx.heap_mut().clear_members(this)?)
        }
    }
}
pub use implementation::{CLASS, install};
