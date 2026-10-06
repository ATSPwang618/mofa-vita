//! AlphaMovie's script-driven AMV player; no platform video handles or timers.
use krkr_engine::{
    extensions,
    protocol::{
        graphics::Size,
        pixels::{Bytes, Pixels},
    },
    storages,
};
use krkr_image::amv::Movie;
use std::sync::{Arc, atomic::Ordering};
use tjs_bind::{RestArgs, Utf16, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};
krkr_engine::native_plugin! { pub(crate) AlphaMovie { names: ["AlphaMovie.dll", "AlphaMovie.tpm"], classes: [bindings], extensions: [] } }
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
#[tjs_bind::class(name = "AlphaMovie")]
mod bindings {
    use super::*;
    #[derive(tjs_bind::Trace)]
    pub struct State {
        #[trace(skip = "indexed AMV metadata contains no VM handles")]
        pub(super) movie: Option<Arc<Movie>>,
        #[trace(skip = "budgeted CPU frame contains no VM handles")]
        pub(super) canvas: Option<Arc<Pixels>>,
        pub(super) generation: u64,
        pub(super) playing: bool,
        pub(super) count: i32,
        pub(super) frame: i32,
        pub(super) looping: bool,
        pub(super) next_loop: bool,
        pub(super) preload: i32,
        pub(super) left: i32,
        pub(super) top: i32,
        pub(super) width: i32,
        pub(super) height: i32,
        pub(super) scale: f64,
        pub(super) rate: f64,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                movie: None,
                canvas: None,
                generation: 0,
                playing: false,
                count: 0,
                frame: 0,
                looping: false,
                next_loop: false,
                preload: 0,
                left: 0,
                top: 0,
                width: 0,
                height: 0,
                scale: 1.,
                rate: 1.,
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(_args: RestArgs<'_>) -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method]
        pub(super) fn clear(&mut self) {
            self.movie = None;
            self.canvas = None;
            self.count = 0;
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.clear();
        }
        #[tjs::method(resumable = true)]
        fn open(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
            let owner = cx.this();
            let generation = with_state(cx, owner, |s| {
                s.clear();
                s.generation
            })?;
            storages::managed::plans(
                cx,
                vec![(name.0, true)],
                Opened { owner, generation },
                |next, cx, mut plans| {
                    let plan = Arc::new(plans.pop().flatten().expect("required AMV plan"));
                    extensions::run_work(
                        cx,
                        move |stop| {
                            Movie::open(plan, &|| stop.load(Ordering::Acquire)).map_err(error)
                        },
                        Box::new(next),
                    )
                },
            )
        }
        #[tjs::method(name = "showNextImage", resumable = true)]
        fn show(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
            show_next(cx, layer)
        }
        #[tjs::method(name = "isPlaying")]
        fn is_playing(&self) -> bool {
            self.playing
        }
        #[tjs::method]
        fn play(&mut self) {
            self.playing = true;
        }
        #[tjs::method]
        fn stop(&mut self) {
            self.clear();
            self.playing = false;
            self.frame = 0;
        }
        #[tjs::method(name = "setPosition")]
        fn position(&mut self, #[tjs(coerce)] x: i32, #[tjs(coerce)] y: i32) {
            self.left = x;
            self.top = y;
        }
        #[tjs::method(name = "setNextMovieFile")]
        fn next_file(&self, _name: Utf16) { /* Reference implementation does nothing. */
        }
        #[tjs::getter(name = "numOfFrame")]
        fn count(&self) -> i64 {
            i64::from(self.count)
        }
        #[tjs::setter(name = "numOfFrame")]
        fn set_count(&mut self, #[tjs(coerce)] value: i32) {
            self.count = value;
        }
        #[tjs::getter]
        fn frame(&self) -> i64 {
            i64::from(self.frame)
        }
        #[tjs::setter(name = "frame")]
        fn set_frame(&mut self, #[tjs(coerce)] value: i32) {
            self.frame = value;
        }
        #[tjs::getter(name = "loop")]
        fn looping(&self) -> bool {
            self.looping
        }
        #[tjs::setter(name = "loop")]
        fn set_loop(&mut self, #[tjs(coerce)] value: bool) {
            self.looping = value;
        }
        #[tjs::getter(name = "nextLoop")]
        fn next_loop(&self) -> bool {
            self.next_loop
        }
        #[tjs::setter(name = "nextLoop")]
        fn set_next_loop(&mut self, #[tjs(coerce)] value: bool) {
            self.next_loop = value;
        }
        #[tjs::getter(name = "preloadSamples")]
        fn preload(&self) -> i64 {
            i64::from(self.preload)
        }
        #[tjs::setter(name = "preloadSamples")]
        fn set_preload(&mut self, #[tjs(coerce)] value: i32) {
            self.preload = value;
        }
        #[tjs::getter]
        fn left(&self) -> i64 {
            i64::from(self.left)
        }
        #[tjs::setter(name = "left")]
        fn set_left(&mut self, #[tjs(coerce)] value: i32) {
            self.left = value;
        }
        #[tjs::getter]
        fn top(&self) -> i64 {
            i64::from(self.top)
        }
        #[tjs::setter(name = "top")]
        fn set_top(&mut self, #[tjs(coerce)] value: i32) {
            self.top = value;
        }
        #[tjs::getter(name = "screenWidth")]
        fn width(&self) -> i64 {
            i64::from(self.width)
        }
        #[tjs::setter(name = "screenWidth")]
        fn set_width(&mut self, #[tjs(coerce)] value: i32) {
            self.width = value;
        }
        #[tjs::getter(name = "screenHeight")]
        fn height(&self) -> i64 {
            i64::from(self.height)
        }
        #[tjs::setter(name = "screenHeight")]
        fn set_height(&mut self, #[tjs(coerce)] value: i32) {
            self.height = value;
        }
        #[tjs::getter(name = "FPSScale")]
        fn scale(&self) -> f64 {
            self.scale
        }
        #[tjs::setter(name = "FPSScale")]
        fn set_scale(&mut self, #[tjs(coerce)] value: f64) {
            self.scale = value;
        }
        #[tjs::getter(name = "FPSRate")]
        fn rate(&self) -> f64 {
            self.rate
        }
        #[tjs::setter(name = "FPSRate")]
        fn set_rate(&mut self, #[tjs(coerce)] value: f64) {
            self.rate = value;
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Opened {
    owner: ObjId,
    generation: u64,
}
impl extensions::WorkContinuation<Option<Movie>> for Opened {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        movie: Option<Movie>,
    ) -> NativeResult<NativeStep> {
        bindings::with_state(cx, self.owner, |s| {
            if s.generation != self.generation {
                return;
            }
            if let Some(movie) = movie {
                s.count = movie.count() as i32;
                s.rate = movie.rate as f64;
                s.width = movie.size.width as i32;
                s.height = movie.size.height as i32;
                s.frame = 0;
                s.movie = Some(Arc::new(movie));
            }
        })?;
        Ok(NativeStep::Return(Value::Void))
    }
}
#[derive(tjs_bind::Trace)]
struct Shown {
    owner: ObjId,
    layer: Value,
    generation: u64,
    frame: i32,
    width: i32,
    height: i32,
}
fn show_next(cx: &mut NativeCx<'_>, layer: Value) -> NativeResult<NativeStep> {
    let budget = extensions::layer_pixel_budget(cx, layer)?;
    let owner = cx.this();
    let (movie, canvas, left, top, next) = bindings::with_state(cx, owner, |s| {
        if let Some(movie) = &s.movie {
            s.frame = s.frame.wrapping_add(1);
            if i64::from(s.frame) > movie.count() as i64 {
                s.frame = 1;
            }
        }
        (
            s.movie.clone(),
            s.canvas.clone(),
            s.left,
            s.top,
            Shown {
                owner,
                layer,
                generation: s.generation,
                frame: s.frame,
                width: s.width,
                height: s.height,
            },
        )
    })?;
    if next.width < 0 || next.height < 0 {
        return Err(NativeError::Message("negative AMV display size"));
    }
    let Some(movie) = movie else {
        let step = extensions::layer_resize_image_and_layer(cx, layer, next.width, next.height)?;
        return Ok(flow::returning(step, Value::Int(next.frame as i64)));
    };
    if next.frame <= 0 {
        return Err(NativeError::Message("AMV frame index out of bounds"));
    }
    let index = next.frame as usize - 1;
    extensions::run_work(
        cx,
        move |stop| {
            let cancelled = || stop.load(Ordering::Acquire);
            let (size, data) = movie.decode(index, &budget, &cancelled).map_err(error)?;
            let length = movie
                .size
                .rgba_bytes()
                .ok_or(NativeError::Message("AMV canvas size overflow"))?;
            let mut pixels = Bytes::zeroed(length, &budget).map_err(error)?;
            if let Some(canvas) = &canvas
                && let Some(main) = &canvas.main
            {
                pixels.as_mut_slice().copy_from_slice(main.as_slice());
            }
            let w = movie.size.width as i64;
            let h = movie.size.height as i64;
            let x0 = i64::from(left).max(0).min(w);
            let y0 = i64::from(top).max(0).min(h);
            let x1 = (i64::from(left) + i64::from(size.width)).clamp(x0, w);
            let y1 = (i64::from(top) + i64::from(size.height)).clamp(y0, h);
            for y in y0..y1 {
                if cancelled() {
                    return Err(NativeError::Message("AMV frame cancelled"));
                }
                let src = ((y - i64::from(top)) * i64::from(size.width) + x0 - i64::from(left))
                    as usize
                    * 4;
                let dst = (y * w + x0) as usize * 4;
                let n = (x1 - x0) as usize * 4;
                pixels.as_mut_slice()[dst..dst + n].copy_from_slice(&data.as_slice()[src..src + n]);
            }
            Ok(Arc::new(Pixels {
                size: movie.size,
                main: Some(pixels),
                province: None,
            }))
        },
        Box::new(next),
    )
}
impl extensions::WorkContinuation<Arc<Pixels>> for Shown {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        let current = bindings::with_state(cx, self.owner, |s| {
            if s.generation != self.generation {
                return false;
            }
            s.canvas = Some(pixels.clone());
            true
        })?;
        if !current {
            return Ok(NativeStep::Return(Value::Int(self.frame as i64)));
        }
        let step = extensions::layer_write_shared_pixels(
            cx,
            self.layer,
            pixels,
            Size {
                width: self.width as u32,
                height: self.height as u32,
            },
        )?;
        Ok(flow::returning(step, Value::Int(self.frame as i64)))
    }
}
