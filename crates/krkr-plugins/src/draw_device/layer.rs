use tjs_bind::RestArgs;
use tjs_core::{NativeCx, NativeError, NativeResult, Value, value};

#[tjs_bind::class(name = "D3DLayer")]
pub(crate) mod bindings {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        pub device: Value,
        pub pictures: Vec<Value>,
        #[trace(skip = "Managed GPU image lease")]
        pub emote: Option<krkr_engine::extensions::GpuImage>,
        pub emote_generation: u64,
        pub matrix: [f32; 16],
        pub plane: i32,
        pub front: i32,
        pub back: i32,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                device: Value::Void,
                pictures: Vec::new(),
                emote: None,
                emote_generation: 0,
                plane: 0,
                front: 0,
                back: 0,
                matrix: [
                    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
                ],
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, device: Value) -> Self {
            let owner = cx.this();
            let valid = crate::exports::object(device).ok().is_some_and(|id| {
                super::super::device::bindings::with_state(cx, id, |s| {
                    s.layers.push(Value::Obj(owner.into()))
                })
                .is_ok()
            });
            Self {
                device: if valid { device } else { Value::Void },
                ..Default::default()
            }
        }
        #[tjs::method(name = "setMatrix")]
        fn matrix(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            if args.len() < 16 {
                return Err(NativeError::Missing(args.len()));
            }
            let mut matrix = [0.; 16];
            for (i, v) in matrix.iter_mut().enumerate() {
                *v = value::to_real(cx.heap(), args[i])? as f32;
            }
            cx.with_state::<Self, _>(|s, _| {
                s.matrix = matrix;
                Ok(())
            })
        }
        #[tjs::getter(name = "drawPlane")]
        fn plane(&self) -> i64 {
            i64::from(self.plane)
        }
        #[tjs::setter(name = "drawPlane")]
        fn set_plane(&mut self, #[tjs(coerce)] v: i32) {
            self.plane = v;
        }
        #[tjs::getter(name = "frontIndex")]
        fn front(&self) -> i64 {
            i64::from(self.front)
        }
        #[tjs::setter(name = "frontIndex")]
        fn set_front(&mut self, #[tjs(coerce)] v: i32) {
            self.front = v;
        }
        #[tjs::getter(name = "backIndex")]
        fn back(&self) -> i64 {
            i64::from(self.back)
        }
        #[tjs::setter(name = "backIndex")]
        fn set_back(&mut self, #[tjs(coerce)] v: i32) {
            self.back = v;
        }
        #[tjs::method]
        fn finalize(&self) {} // Reference leaves ownership to the native destructor.
        #[tjs::invalidate]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let owner = cx.this();
            let (device, pictures) = cx.with_state::<Self, _>(|s, _| {
                s.emote = None;
                s.emote_generation = s.emote_generation.wrapping_add(1);
                Ok((
                    std::mem::replace(&mut s.device, Value::Void),
                    std::mem::take(&mut s.pictures),
                ))
            })?;
            if let Ok(id) = crate::exports::object(device) {
                let _ = super::super::device::bindings::with_state(cx, id, |s| {
                    s.layers
                        .retain(|v| !matches!(v, Value::Obj(r) if r.object == Some(owner)))
                });
            }
            for picture in pictures {
                if let Ok(id) = crate::exports::object(picture) {
                    let _ = super::super::picture::bindings::with_state(cx, id, |s| {
                        s.layer = Value::Void
                    });
                }
            }
            Ok(())
        }
    }
}
