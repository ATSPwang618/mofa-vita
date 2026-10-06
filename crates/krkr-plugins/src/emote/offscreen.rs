//! D3DAdaptor is the reference's independent readback canvas, despite its
//! historical name. Canvas storage stays on the graphics backend.
use extensions::GpuImage;
use krkr_engine::{
    extensions,
    protocol::{budget::Budget, graphics::Size},
};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Value};

#[tjs_bind::class(name = "Motion.D3DAdaptor")]
pub(super) mod bindings {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        #[trace(skip = "Managed GPU image lease")]
        pub image: Option<GpuImage>,
        #[trace(skip = "Shared memory accounting")]
        pub budget: Budget,
        #[trace(skip = "Canvas pixel dimensions")]
        pub size: Size,
        pub origin: [i32; 2],
        pub generation: u64,
        pub clear_color: u32,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                image: None,
                budget: Budget::new(0),
                size: Size {
                    width: 0,
                    height: 0,
                },
                origin: [0; 2],
                generation: 0,
                clear_color: 0,
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(
            cx: &mut NativeCx<'_>,
            _window: Value,
            #[tjs(coerce)] width: i32,
            #[tjs(coerce)] height: i32,
            #[tjs(coerce)] x: i32,
            #[tjs(coerce)] y: i32,
        ) -> NativeResult<Self> {
            if width <= 0 || height <= 0 {
                return Err(NativeError::Message("invalid E-mote canvas size"));
            }
            let size = Size {
                width: width as u32,
                height: height as u32,
            };
            let budget = extensions::image_staging_budget(cx.heap_mut())?
                .unwrap_or_else(|| Budget::new(64 * 1024 * 1024));
            Ok(Self {
                image: None,
                budget,
                size,
                origin: [x, y],
                generation: 0,
                clear_color: 0,
            })
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.image = None;
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::method(name = "setClearColor")]
        fn set_clear_color(&mut self, #[tjs(coerce)] color: i64) {
            // Reference stores this value; unloadUnusedTextures clears to zero.
            self.clear_color = color as u32;
        }
        #[tjs::method(name = "unloadUnusedTextures")]
        fn clear(&mut self) -> NativeResult<()> {
            self.image = None;
            self.generation = self.generation.wrapping_add(1);
            Ok(())
        }
        #[tjs::method(name = "captureCanvas", resumable = true)]
        fn capture(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
            let (pixels, size) = cx.with_state::<Self, _>(|s, _| Ok((s.image.clone(), s.size)))?;
            if extensions::layer_size(cx, layer)? != size {
                return Err(NativeError::Message("E-mote capture canvas size mismatch"));
            }
            let Some(pixels) = pixels else {
                let target = extensions::gpu_image(cx, size)?;
                return extensions::draw_meshes(
                    cx,
                    target,
                    krkr_engine::protocol::mesh::Batch {
                        clear: Some([0.; 4]),
                        ..Default::default()
                    },
                    Box::new(Capture { layer }),
                );
            };
            extensions::layer_patch_image(cx, layer, pixels)
        }
    }
}

#[derive(tjs_bind::Trace)]
struct Capture {
    layer: Value,
}
impl extensions::ImageContinuation for Capture {
    fn image(self: Box<Self>, cx: &mut NativeCx<'_>, image: GpuImage) -> NativeResult<NativeStep> {
        extensions::layer_patch_image(cx, self.layer, image)
    }
}
