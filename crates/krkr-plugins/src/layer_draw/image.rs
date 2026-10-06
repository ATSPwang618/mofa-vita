use super::{
    geometry,
    matrix::Matrix,
    path::{Path, Segment},
    raster::{Draw, Texture},
};
use krkr_engine::{
    extensions,
    protocol::{budget::Budget, graphics::Size},
    storages,
};
use std::sync::Arc;
use tjs_bind::{RestArgs, Utf16};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value};
pub trait Reply: Trace {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        image: Option<Image>,
    ) -> NativeResult<NativeStep>;
}
pub fn resolve(
    cx: &mut NativeCx<'_>,
    input: Value,
    next: Box<dyn Reply>,
) -> NativeResult<NativeStep> {
    if let Value::Str(id) = input {
        let name = cx.heap().string(id)?.to_vec();
        return storages::managed::plans(
            cx,
            vec![(name, false)],
            Resolved(next),
            |s, cx, mut plans| {
                let Some(plan) = plans.pop().flatten() else {
                    return s.0.resume(cx, None);
                };
                let budget = extensions::image_staging_budget(cx.heap_mut())?
                    .unwrap_or_else(|| Budget::new(64 * 1024 * 1024));
                let request = krkr_image::Request::from_plans(
                    plan, None, None, 0x02ffffff, None, false, budget,
                );
                extensions::run_work(
                    cx,
                    move |stop| {
                        let mut pixels = request
                            .probe(stop)
                            .map_err(|e| NativeError::Detail(e.to_string()))?
                            .decode(stop)
                            .map_err(|e| NativeError::Detail(e.to_string()))?
                            .pixels;
                        let bytes = pixels
                            .main
                            .take()
                            .ok_or(NativeError::Message("image has no color plane"))?;
                        let texture = Texture::from_rgba(bytes, pixels.size)
                            .ok_or(NativeError::Message("invalid image dimensions"))?;
                        Ok(Image {
                            size: texture.size,
                            texture: Some(Arc::new(texture)),
                            records: Vec::new(),
                            matrix: Matrix::default(),
                            background: 0,
                        })
                    },
                    Box::new(s),
                )
            },
        );
    }
    if let Value::Obj(reference) = input
        && let Some(id) = reference.object
    {
        if let Ok(image) = cx
            .heap_mut()
            .with_native_state::<bindings::State, _>(id, |s| s.image.clone())
        {
            return next.resume(cx, image);
        }
        if cx
            .heap_mut()
            .with_native_state::<super::layer::State, _>(id, |_| ())
            .is_ok()
        {
            let budget = extensions::layer_pixel_budget(cx, input)?;
            return extensions::layer_read_pixels(cx, input, Box::new(LayerImage { next, budget }));
        }
    }
    next.resume(cx, None)
}
struct Resolved(Box<dyn Reply>);
impl Trace for Resolved {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.0.trace(v);
    }
}
impl extensions::WorkContinuation<Image> for Resolved {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, image: Image) -> NativeResult<NativeStep> {
        self.0.resume(cx, Some(image))
    }
}
struct LayerImage {
    next: Box<dyn Reply>,
    budget: Budget,
}
impl Trace for LayerImage {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.next.trace(v);
    }
}
impl extensions::PixelContinuation for LayerImage {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_engine::protocol::pixels::Pixels>,
    ) -> NativeResult<NativeStep> {
        let Self { next, budget } = *self;
        extensions::run_work(
            cx,
            move |stop| {
                let data = pixels
                    .main
                    .as_ref()
                    .ok_or(NativeError::Message("layer has no color plane"))?;
                let mut bytes =
                    krkr_engine::protocol::pixels::Bytes::zeroed(data.as_slice().len(), &budget)
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                for (to, from) in bytes
                    .as_mut_slice()
                    .chunks_mut(65536)
                    .zip(data.as_slice().chunks(65536))
                {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(NativeError::Message("image conversion cancelled"));
                    }
                    to.copy_from_slice(from);
                }
                let texture = Texture::from_rgba(bytes, pixels.size)
                    .ok_or(NativeError::Message("invalid layer dimensions"))?;
                Ok(Image {
                    size: texture.size,
                    texture: Some(Arc::new(texture)),
                    records: Vec::new(),
                    matrix: Matrix::default(),
                    background: 0,
                })
            },
            Box::new(Resolved(next)),
        )
    }
}
#[derive(Clone)]
pub struct Record {
    pub path: Path,
    pub appearance: Vec<Draw>,
}
#[derive(Clone)]
pub struct Image {
    pub size: Size,
    pub texture: Option<Arc<Texture>>,
    pub records: Vec<Record>,
    pub matrix: Matrix,
    pub background: u32,
}
impl Image {
    pub fn vector(size: Size) -> Self {
        Self {
            size,
            texture: None,
            records: Vec::new(),
            matrix: Matrix::default(),
            background: 0,
        }
    }
    pub fn duplicate(&self) -> Self {
        let mut image = self.clone();
        image.background = 0;
        if image.texture.is_some() {
            image.matrix = Matrix::default();
        }
        image
    }
    pub fn bounds(&self) -> [f64; 4] {
        if self.texture.is_some() {
            return [
                0.,
                0.,
                f64::from(self.size.width),
                f64::from(self.size.height),
            ];
        }
        let mut bounds = [0f32; 4];
        for record in &self.records {
            for segment in &record.path.segments {
                let points: &[[f64; 2]] = match segment {
                    Segment::Move(p) | Segment::Line(p) => std::slice::from_ref(p),
                    Segment::Cubic(a, b, c) => {
                        for p in [a, b, c] {
                            extend(&mut bounds, *p);
                        }
                        continue;
                    }
                    Segment::Close => continue,
                };
                for p in points {
                    extend(&mut bounds, *p);
                }
            }
        }
        [
            f64::from(bounds[0]),
            f64::from(bounds[1]),
            f64::from(bounds[2] - bounds[0]),
            f64::from(bounds[3] - bounds[1]),
        ]
    }
}
fn extend(bounds: &mut [f32; 4], p: [f64; 2]) {
    let [x, y] = p.map(|v| v as f32);
    bounds[0] = bounds[0].min(x);
    bounds[1] = bounds[1].min(y);
    bounds[2] = bounds[2].max(x);
    bounds[3] = bounds[3].max(y);
}
pub fn make(cx: &mut NativeCx<'_>, image: Image) -> NativeResult<Value> {
    let class = cx
        .heap()
        .registered_class("GdiPlus.Image")
        .ok_or(NativeError::Message("Image class is not installed"))?;
    Ok(Value::Obj(
        cx.heap_mut()
            .alloc_native(class, bindings::State { image: Some(image) })?
            .into(),
    ))
}
#[tjs_bind::class(name = "GdiPlus.Image")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) image: Option<Image>,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        fn get(&self) -> NativeResult<&Image> {
            self.image.as_ref().ok_or(NativeError::This)
        }
        #[tjs::constructor(resumable = true)]
        fn construct(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.is_empty() {
                return Ok(NativeStep::Return(cx.construct(Self::default())?));
            }
            let Value::Str(name) = args[0] else {
                return Err(NativeError::Type("an image filename"));
            };
            let name = cx.heap().string(name)?.to_vec();
            load(cx, name, true)
        }
        #[tjs::method(resumable = true)]
        fn load(cx: &mut NativeCx<'_>, filename: Utf16) -> NativeResult<NativeStep> {
            super::load(cx, filename.0, false)
        }
        #[tjs::method(name = "Clone")]
        fn duplicate(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            make(cx, self.get()?.duplicate())
        }
        #[tjs::method(name = "GetBounds")]
        fn bounds(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            geometry::rectangle(cx, self.get()?.bounds())
        }
        #[tjs::method(name = "GetWidth")]
        fn width(&self) -> NativeResult<i64> {
            Ok(i64::from(self.get()?.size.width))
        }
        #[tjs::method(name = "GetHeight")]
        fn height(&self) -> NativeResult<i64> {
            Ok(i64::from(self.get()?.size.height))
        }
        #[tjs::method(name = "GetFlags")]
        fn flags(&self) -> NativeResult<i64> {
            Ok(if self.get()?.texture.is_some() {
                0x12
            } else {
                0x10010
            })
        }
        #[tjs::method(name = "GetType")]
        fn kind(&self) -> NativeResult<i64> {
            Ok(if self.get()?.texture.is_some() { 1 } else { 2 })
        }
        #[tjs::method(name = "GetLastStatus")]
        fn status(&self) -> NativeResult<i64> {
            self.get()?;
            Ok(0)
        }
        #[tjs::method(name = "GetPixelFormat")]
        fn format(&self) -> NativeResult<i64> {
            self.get()?;
            Ok(0x26200a)
        }
        #[tjs::method(name = "GetHorizontalResolution")]
        fn horizontal(&self) -> NativeResult<f64> {
            self.get()?;
            Ok(96.)
        }
        #[tjs::method(name = "GetVerticalResolution")]
        fn vertical(&self) -> NativeResult<f64> {
            self.get()?;
            Ok(96.)
        }
        #[tjs::method(name = "RotateFlip")]
        fn rotate(&mut self, #[tjs(coerce)] mode: i32) -> NativeResult<i64> {
            let image = self.image.as_mut().ok_or(NativeError::This)?;
            let angle = match mode {
                0 => return Ok(0),
                1 => Some(std::f32::consts::FRAC_PI_2),
                2 => Some(std::f32::consts::PI),
                3 => Some((std::f64::consts::PI * 1.5) as f32),
                4 | 6 => None,
                _ => return Ok(6),
            };
            image.matrix = if let Some(angle) = angle {
                let (sn, cs) = angle.sin_cos();
                Matrix::new([cs, sn, -sn, cs, 0., 0.])
            } else if mode == 4 {
                Matrix::new([-1., 0., 0., 1., 0., 0.])
            } else {
                Matrix::new([1., 0., 0., -1., 0., 0.])
            };
            Ok(0)
        }
    }
}
fn load(cx: &mut NativeCx<'_>, name: Vec<u16>, construct: bool) -> NativeResult<NativeStep> {
    let budget = extensions::image_staging_budget(cx.heap_mut())?
        .unwrap_or_else(|| Budget::new(64 * 1024 * 1024));
    storages::managed::plans(
        cx,
        vec![(name, true)],
        Loaded {
            owner: cx.this(),
            construct,
            budget,
        },
        |s, cx, mut plans| {
            let plan = plans.pop().flatten().expect("required image plan");
            let request = krkr_image::Request::from_plans(
                plan,
                None,
                None,
                0x02ffffff,
                None,
                false,
                s.budget.clone(),
            );
            extensions::run_work(
                cx,
                move |stop| {
                    let mut pixels = request
                        .probe(stop)
                        .map_err(|e| NativeError::Detail(e.to_string()))?
                        .decode(stop)
                        .map_err(|e| NativeError::Detail(e.to_string()))?
                        .pixels;
                    let texture = Texture::from_rgba(
                        pixels
                            .main
                            .take()
                            .ok_or(NativeError::Message("image has no main plane"))?,
                        pixels.size,
                    )
                    .ok_or(NativeError::Message("invalid image dimensions"))?;
                    Ok(Image {
                        size: texture.size,
                        texture: Some(Arc::new(texture)),
                        records: Vec::new(),
                        matrix: Matrix::default(),
                        background: 0,
                    })
                },
                Box::new(s),
            )
        },
    )
}
struct Loaded {
    owner: ObjId,
    construct: bool,
    budget: Budget,
}
impl Trace for Loaded {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.owner.trace(v);
    }
}
impl extensions::WorkContinuation<Image> for Loaded {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, image: Image) -> NativeResult<NativeStep> {
        if self.construct {
            return Ok(NativeStep::Return(
                cx.construct(bindings::State { image: Some(image) })?,
            ));
        }
        cx.heap_mut()
            .with_native_state::<bindings::State, _>(self.owner, |s| s.image = Some(image))?;
        Ok(NativeStep::Return(Value::Void))
    }
}
