//! Legacy drawer selection is compatibility state. Rendering remains on the
//! platform host; dtDrawDib is a script token, not a request to use Win32 GDI.
use super::*;

#[tjs_bind::class(name = "BasicDrawDevice")]
mod implementation {
    use super::*;

    #[derive(tjs_bind::Trace)]
    pub struct State {
        preferred: i32,
    }
    impl Default for State {
        fn default() -> Self {
            Self { preferred: 1 }
        }
    }
    impl State {
        #[tjs::constructor]
        fn create() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let device = cx.this();
            let window = cx
                .heap_mut()
                .with_native_state::<Interface, _>(device, |s| s.window)
                .unwrap_or(None);
            if let Some(window) = window {
                detach(cx, Value::Obj(window.into()), device)?;
            }
            Ok(())
        }
        #[tjs::method]
        fn recreate(&self) {}
        #[tjs::getter(name = "interface")]
        fn interface(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            let owner = cx.this();
            register(cx.heap_mut(), owner, NativeCallable::Leaf(attach))
        }
        #[tjs::getter(name = "preferredDrawer")]
        fn preferred(&self) -> i64 {
            self.preferred.into()
        }
        #[tjs::setter(name = "preferredDrawer")]
        fn set_preferred(&mut self, value: i64) {
            self.preferred = value as i32;
        }
        #[tjs::getter]
        fn drawer(&self) -> i64 {
            1
        }
        #[tjs::getter(name = "dtNone")]
        fn none() -> i64 {
            0
        }
        #[tjs::getter(name = "dtDrawDib")]
        fn dib() -> i64 {
            1
        }
        #[tjs::getter(name = "dtDBGDI")]
        fn gdi() -> i64 {
            2
        }
        #[tjs::getter(name = "dtDBDD")]
        fn direct_draw() -> i64 {
            3
        }
        #[tjs::getter(name = "dtDBD3D")]
        fn direct_3d() -> i64 {
            4
        }
    }
}

fn attach(_: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    // The built-in device uses the Window's ordinary Layer composition.
    Ok(Value::Void)
}

pub(in crate::window) fn install(heap: &mut Heap, window: ObjId) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    for name in ["BasicDrawDevice", "PassThroughDrawDevice"] {
        let key = heap.intern_str(name);
        heap.set_member_flags(window, key, Value::Obj(ObjRef::bound(class)), false, true)?;
    }
    Ok(())
}

pub(in crate::window) fn create_default(cx: &mut NativeCx<'_>) -> NativeResult<()> {
    let window = cx.this();
    let class = cx
        .heap()
        .registered_class("BasicDrawDevice")
        .ok_or(NativeError::This)?;
    let device = cx
        .heap_mut()
        .alloc_native(class, implementation::State::default())?;
    let handle = register(cx.heap_mut(), device, NativeCallable::Leaf(attach))?;
    cx.heap_mut()
        .with_native_state::<Interface, _>(device, |s| s.window = Some(window))?;
    let object = Value::Obj(ObjRef::bound(device));
    cx.heap_mut().initialize_native_state(
        window,
        Binding {
            handle,
            object,
            device: object,
            revision: 0,
        },
    )
}
