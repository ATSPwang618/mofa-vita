use tjs_core::{NativeCx, NativeResult, Value};

#[tjs_bind::class(name = "D3DPicture")]
pub(super) mod bindings {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        pub layer: Value,
        pub image: Value,
        pub source: [i32; 4],
        pub destination: [i32; 2],
        pub coordinate: [f32; 2],
        pub blend: i32,
        pub opacity: i32,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                layer: Value::Void,
                image: Value::Void,
                source: [0; 4],
                destination: [0; 2],
                coordinate: [0.; 2],
                blend: 2,
                opacity: 255,
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, layer: Value, image: Value) -> Self {
            let owner = cx.this();
            let layer_valid = crate::exports::object(layer).ok().is_some_and(|id| {
                super::super::layer::bindings::with_state(cx, id, |s| {
                    s.pictures.push(Value::Obj(owner.into()))
                })
                .is_ok()
            });
            let image_valid = crate::exports::object(image).ok().is_some_and(|id| {
                super::super::image::bindings::with_state(cx, id, |_| ()).is_ok()
            });
            Self {
                layer: if layer_valid { layer } else { Value::Void },
                image: if image_valid { image } else { Value::Void },
                ..Default::default()
            }
        }
        #[tjs::method(name = "assignImageRange")]
        fn range(
            &mut self,
            #[tjs(coerce)] sx: i32,
            #[tjs(coerce)] sy: i32,
            #[tjs(coerce)] sw: i32,
            #[tjs(coerce)] sh: i32,
            #[tjs(coerce)] dx: i32,
            #[tjs(coerce)] dy: i32,
        ) {
            self.source = [sx, sy, sw, sh];
            self.destination = [dx, dy];
        }
        #[tjs::method(name = "setCoord")]
        fn coord(&mut self, x: f64, y: f64) {
            self.coordinate = [x as f32, y as f32];
        }
        #[tjs::getter(name = "blendMode")]
        fn blend(&self) -> i64 {
            i64::from(self.blend)
        }
        #[tjs::setter(name = "blendMode")]
        fn set_blend(&mut self, #[tjs(coerce)] v: i32) {
            self.blend = v;
        }
        #[tjs::getter]
        fn opacity(&self) -> i64 {
            i64::from(self.opacity)
        }
        #[tjs::setter(name = "opacity")]
        fn set_opacity(&mut self, #[tjs(coerce)] v: i32) {
            self.opacity = v;
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let owner = cx.this();
            let layer = cx.with_state::<Self, _>(|s, _| {
                s.image = Value::Void;
                Ok(std::mem::replace(&mut s.layer, Value::Void))
            })?;
            if let Ok(id) = crate::exports::object(layer) {
                let _ = super::super::layer::bindings::with_state(cx, id, |s| {
                    s.pictures
                        .retain(|v| !matches!(v, Value::Obj(r) if r.object == Some(owner)))
                });
            }
            Ok(())
        }
    }
}
