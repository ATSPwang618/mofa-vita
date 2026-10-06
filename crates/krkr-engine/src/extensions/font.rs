//! Shared font discovery and bounded source loading for vector-text plugins.
use super::{WorkContinuation, run_work};
use crate::protocol::text::Font;
pub use krkr_render::font::Face as FontFace;
use std::sync::{Arc, Mutex};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Trace, Value};
struct Resolve {
    font: Font,
    system: Arc<Mutex<krkr_render::font::System>>,
    next: Box<dyn WorkContinuation<Arc<FontFace>>>,
}
impl Trace for Resolve {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.next.trace(v);
    }
}
impl Resolve {
    fn start(
        self,
        cx: &mut NativeCx<'_>,
        plan: Option<krkr_assets::ReadPlan>,
    ) -> NativeResult<NativeStep> {
        run_work(
            cx,
            move |stop| {
                let mut system = self
                    .system
                    .lock()
                    .map_err(|_| NativeError::Message("font service lock poisoned"))?;
                if let Some(plan) = plan {
                    let permit = system
                        .reserve(plan.bytes as usize)
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                    let bytes = plan
                        .read_interruptible(0, || stop.load(std::sync::atomic::Ordering::Relaxed))
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                    let face = FontFace::from_bytes(bytes, 0, permit)
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                    // The caller owns private file faces; do not pin them in the
                    // engine cache or replace a Layer font with the same filename.
                    return Ok(Arc::new(face));
                }
                system
                    .face(&self.font)
                    .map_err(|e| NativeError::Detail(e.to_string()))
            },
            self.next,
        )
    }
}
pub fn list_font_families(
    cx: &mut NativeCx<'_>,
    next: Box<dyn WorkContinuation<Vec<String>>>,
) -> NativeResult<NativeStep> {
    let system = crate::font::service(cx)?.worker.clone();
    run_work(
        cx,
        move |_| {
            system
                .lock()
                .map_err(|_| NativeError::Message("font service lock poisoned"))?
                .list(&Font::default(), 0)
                .map_err(|e| NativeError::Detail(e.to_string()))
        },
        next,
    )
}

/// Resolve an already selected source without repeating script-backed storage getters.
pub fn resolve_font_source(
    cx: &mut NativeCx<'_>,
    font: Font,
    plan: Option<krkr_assets::ReadPlan>,
    next: Box<dyn WorkContinuation<Arc<FontFace>>>,
) -> NativeResult<NativeStep> {
    let system = crate::font::service(cx)?.worker.clone();
    Resolve { font, system, next }.start(cx, plan)
}
