//! A draw uses the engine's ordered readback, bounded worker, then main-plane
//! patch. Layer geometry and province pixels remain owned by the engine.
use super::{
    matrix::Matrix,
    path::Path,
    properties::get,
    raster::{self, Draw},
};
use krkr_engine::{
    extensions,
    protocol::{
        budget::Budget,
        graphics::{Rect, Size},
        pixels::{Bytes, Pixels},
    },
};
use std::sync::{Arc, atomic::Ordering};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, value};

pub struct Drawing {
    pub path: Path,
    pub appearance: Vec<Draw>,
    pub matrix: Matrix,
    pub antialias: bool,
    pub update: bool,
}
pub fn start(
    cx: &mut NativeCx<'_>,
    layer: Value,
    drawing: Drawing,
    result: Value,
) -> NativeResult<NativeStep> {
    let update = drawing.update;
    batch(
        cx,
        layer,
        Batch {
            drawings: vec![drawing],
            clear: None,
            update,
            whole: false,
        },
        result,
    )
}
pub struct Batch {
    pub drawings: Vec<Drawing>,
    pub clear: Option<u32>,
    pub update: bool,
    pub whole: bool,
}
// Fill control bounds contain the complete transformed curve. Keep a two-pixel
// rasterization margin. Strokes (including custom caps) use the complete clip
// until their expanded outline can be bounded without changing coverage.
fn region(batch: &Batch, clip: [i32; 4], size: Size) -> Option<Rect> {
    let clip = size.rect().intersection(Rect {
        left: clip[0],
        top: clip[1],
        width: clip[2].max(0) as u32,
        height: clip[3].max(0) as u32,
    })?;
    if batch.clear.is_some() {
        return Some(clip);
    }
    let mut bounds: Option<Rect> = None;
    for item in &batch.drawings {
        let Some(shape) = raster::geometry(&item.path) else {
            continue;
        };
        for draw in &item.appearance {
            if matches!(&draw.drawing, raster::Drawing::Stroke(_)) {
                return Some(clip);
            }
            let [x, y] = draw.offset;
            let m = Matrix::product([1., 0., 0., 1., x, y], item.matrix.elements);
            let transform = tiny_skia::Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]);
            let Some(transformed) = shape.bounds().transform(transform) else {
                return Some(clip);
            };
            let left = (f64::from(transformed.left()).floor() - 2.).max(f64::from(clip.left));
            let top = (f64::from(transformed.top()).floor() - 2.).max(f64::from(clip.top));
            let right = (f64::from(transformed.right()).ceil() + 2.)
                .min(f64::from(clip.left) + f64::from(clip.width));
            let bottom = (f64::from(transformed.bottom()).ceil() + 2.)
                .min(f64::from(clip.top) + f64::from(clip.height));
            if right <= left || bottom <= top {
                continue;
            }
            let next = Rect {
                left: left as i32,
                top: top as i32,
                width: (right - left) as u32,
                height: (bottom - top) as u32,
            };
            bounds = Some(bounds.map_or(next, |old| {
                let left = old.left.min(next.left);
                let top = old.top.min(next.top);
                let right = (i64::from(old.left) + i64::from(old.width))
                    .max(i64::from(next.left) + i64::from(next.width));
                let bottom = (i64::from(old.top) + i64::from(old.height))
                    .max(i64::from(next.top) + i64::from(next.height));
                Rect {
                    left,
                    top,
                    width: (right - i64::from(left)) as u32,
                    height: (bottom - i64::from(top)) as u32,
                }
            }));
        }
    }
    bounds
}
pub fn batch(
    cx: &mut NativeCx<'_>,
    layer: Value,
    drawing: Batch,
    result: Value,
) -> NativeResult<NativeStep> {
    extensions::layer_prepare_draw(cx, layer)?;
    let budget = extensions::layer_pixel_budget(cx, layer)?;
    Read {
        layer,
        drawing,
        result,
        budget,
        clip: [0; 4],
        field: 0,
        size: Size {
            width: 0,
            height: 0,
        },
        rectangle: None,
    }
    .advance(cx)
}
struct Read {
    layer: Value,
    drawing: Batch,
    result: Value,
    budget: Budget,
    clip: [i32; 4],
    field: usize,
    size: Size,
    rectangle: Option<Rect>,
}
impl Trace for Read {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.layer.trace(v);
        self.result.trace(v);
    }
}
impl Read {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let names = ["clipLeft", "clipTop", "clipWidth", "clipHeight"];
        let Some(name) = names.get(self.field) else {
            self.size = extensions::layer_image_size(cx, self.layer)?;
            self.rectangle = region(&self.drawing, self.clip, self.size);
            let Some(rectangle) = self.rectangle else {
                return Written {
                    layer: self.layer,
                    result: self.result,
                    size: self.size,
                    rectangle: self.size.rect(),
                    update: self.drawing.update,
                    whole: self.drawing.whole,
                }
                .finish(cx);
            };
            if self.drawing.clear.is_some() {
                // Source replacement covers this entire clipped region.
                return self.render(cx, None);
            }
            return extensions::layer_read_region(cx, self.layer, rectangle, Box::new(self));
        };
        get(cx, self.layer, name, Value::Int(0), self, |mut s, cx, v| {
            s.clip[s.field] = value::to_integer(cx.heap(), v)? as i32;
            s.field += 1;
            s.advance(cx)
        })
    }
}
impl extensions::PixelContinuation for Read {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        input: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        (*self).render(cx, Some(input))
    }
}
impl Read {
    fn render(self, cx: &mut NativeCx<'_>, input: Option<Arc<Pixels>>) -> NativeResult<NativeStep> {
        let Self {
            layer,
            drawing,
            result,
            budget,
            size: layer_size,
            rectangle,
            ..
        } = self;
        let update = drawing.update;
        let whole = drawing.whole;
        let rectangle = rectangle.expect("resolved drawing region");
        let size = Size {
            width: rectangle.width,
            height: rectangle.height,
        };
        if input.as_ref().is_some_and(|input| input.size != size) {
            return Err(NativeError::Message("invalid vector readback extent"));
        }
        extensions::run_work(
            cx,
            move |stop| {
                let length = size
                    .rgba_bytes()
                    .ok_or(NativeError::Message("vector image dimensions overflow"))?;
                let source = input.as_ref().and_then(|input| input.main.as_ref());
                if source.is_none() && drawing.clear.is_none() {
                    return Err(NativeError::Message("layer has no main image"));
                }
                if source.is_some_and(|source| source.as_slice().len() != length) {
                    return Err(NativeError::Message("invalid layer pixel extent"));
                }
                let mut bytes = Bytes::zeroed(length, &budget)
                    .map_err(|e| NativeError::Detail(e.to_string()))?;
                if let Some(source) = source {
                    for (to, from) in bytes
                        .as_mut_slice()
                        .chunks_mut(64 * 1024)
                        .zip(source.as_slice().chunks(64 * 1024))
                    {
                        if stop.load(Ordering::Relaxed) {
                            return Err(NativeError::Message("vector drawing cancelled"));
                        }
                        to.copy_from_slice(from);
                        raster::premultiply(to);
                    }
                }
                let mut target =
                    tiny_skia::PixmapMut::from_bytes(bytes.as_mut_slice(), size.width, size.height)
                        .ok_or(NativeError::Message("invalid vector target"))?;
                if let Some(argb) = drawing.clear {
                    let mut paint = tiny_skia::Paint {
                        blend_mode: tiny_skia::BlendMode::Source,
                        ..Default::default()
                    };
                    paint.set_color_rgba8(
                        (argb >> 16) as u8,
                        (argb >> 8) as u8,
                        argb as u8,
                        (argb >> 24) as u8,
                    );
                    let rect =
                        tiny_skia::Rect::from_xywh(0., 0., size.width as f32, size.height as f32)
                            .ok_or(NativeError::Message("invalid clear extent"))?;
                    target.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
                }
                for item in &drawing.drawings {
                    raster::draw(
                        &mut target,
                        &item.path,
                        &item.appearance,
                        item.matrix,
                        [rectangle.left, rectangle.top],
                        item.antialias,
                        None,
                        &|| stop.load(Ordering::Relaxed),
                        &budget,
                    )
                    .map_err(NativeError::Message)?;
                }
                for (chunk, row) in bytes.as_mut_slice().chunks_mut(64 * 1024).enumerate() {
                    if stop.load(Ordering::Relaxed) {
                        return Err(NativeError::Message("vector drawing cancelled"));
                    }
                    if let Some(source) = source {
                        let original = &source.as_slice()[chunk * 64 * 1024..][..row.len()];
                        for (pixel, original) in row
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .zip(original.as_chunks::<4>().0.iter())
                        {
                            let mut premultiplied =
                                [original[0], original[1], original[2], original[3]];
                            raster::premultiply(&mut premultiplied);
                            if *pixel == premultiplied {
                                pixel.copy_from_slice(original);
                            } else {
                                raster::unpremultiply(pixel);
                            }
                        }
                    } else {
                        raster::unpremultiply(row);
                    }
                }
                Ok(Pixels {
                    size,
                    main: Some(bytes),
                    province: None,
                })
            },
            Box::new(Written {
                layer,
                result,
                size: layer_size,
                rectangle,
                update,
                whole,
            }),
        )
    }
}
struct Written {
    layer: Value,
    result: Value,
    size: Size,
    rectangle: Rect,
    update: bool,
    whole: bool,
}
impl Trace for Written {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.layer.trace(v);
        self.result.trace(v);
    }
}
impl extensions::WorkContinuation<Pixels> for Written {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, pixels: Pixels) -> NativeResult<NativeStep> {
        if extensions::layer_image_size(cx, self.layer)? != self.size {
            return Err(NativeError::Message(
                "layer resized while vector drawing was pending",
            ));
        }
        let step =
            extensions::layer_patch_region(cx, self.layer, self.rectangle, Arc::new(pixels))?;
        Ok(tjs_bind::flow::then(
            step,
            tjs_bind::flow::callback(*self, |s, cx, _| s.finish(cx)),
        ))
    }
}
impl Written {
    fn finish(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let s = self;
        if !s.update {
            return Ok(NativeStep::Return(s.result));
        }
        get(cx, s.layer, "update", Value::Void, s, |s, _, mut method| {
            if let Value::Obj(ref mut r) = method
                && let Value::Obj(layer) = s.layer
            {
                r.this = layer.object;
            }
            Ok(NativeStep::Call {
                function: method,
                arguments: if s.whole {
                    Vec::new()
                } else {
                    vec![Value::Int(0); 4]
                },
                continuation: tjs_bind::flow::complete(s.result),
            })
        })
    }
}
