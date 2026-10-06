use krkr_engine::extensions::{self, GpuImage};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};

#[tjs_bind::class(name = "D3DImage")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub device: Value,
        #[trace(skip = "Managed GPU image lease")]
        pub image: Option<GpuImage>,
        pub generation: u64,
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, device: Value) -> Self {
            let valid = crate::exports::object(device).ok().is_some_and(|id| {
                super::super::device::bindings::with_state(cx, id, |_| ()).is_ok()
            });
            Self {
                device: if valid { device } else { Value::Void },
                ..Default::default()
            }
        }
        #[tjs::method(resumable = true)]
        fn load(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
            let owner = cx.this();
            let (device, generation) = cx.with_state::<Self, _>(|s, _| {
                s.generation = s.generation.wrapping_add(1);
                Ok((s.device, s.generation))
            })?;
            if matches!(device, Value::Void) || extensions::layer_size(cx, layer).is_err() {
                return Ok(NativeStep::Return(Value::Void));
            }
            extensions::layer_snapshot_image(cx, layer, Box::new(Loaded { owner, generation }))
        }
        #[tjs::getter]
        fn width(&self) -> i64 {
            self.image.as_ref().map_or(0, |p| i64::from(p.size.width))
        }
        #[tjs::getter]
        fn height(&self) -> i64 {
            self.image.as_ref().map_or(0, |p| i64::from(p.size.height))
        }
        #[tjs::method]
        fn finalize(&self) {} // The reference method is distinct from destruction.
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.image = None;
            self.device = Value::Void;
            self.generation = self.generation.wrapping_add(1);
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Loaded {
    owner: ObjId,
    generation: u64,
}
impl extensions::ImageContinuation for Loaded {
    fn image(self: Box<Self>, cx: &mut NativeCx<'_>, pixels: GpuImage) -> NativeResult<NativeStep> {
        let valid = bindings::with_state(cx, self.owner, |s| {
            if s.generation != self.generation {
                return false;
            }
            if pixels.size.width > 0 && pixels.size.height > 0 {
                s.image = Some(pixels);
            }
            true
        })?;
        if !valid {
            return Err(NativeError::Message("D3DImage changed while loading"));
        }
        Ok(NativeStep::Return(Value::Void))
    }
}
