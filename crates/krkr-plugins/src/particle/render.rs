use super::*;
use krkr_engine::protocol::{
    graphics::{Rect, Size},
    sprites::{Sprite, Sprites},
};
use std::sync::Arc;
pub(super) fn clear(cx: &mut NativeCx<'_>, owner: ObjId) -> NativeResult<NativeStep> {
    extensions::layer_clear_main(cx, Value::Obj(owner.into()))
}
#[derive(tjs_bind::Trace)]
struct Drawing {
    owner: ObjId,
    values: Vec<f64>,
    #[trace(skip = "Image dimensions are numeric")]
    size: Size,
}
pub(super) fn start(cx: &mut NativeCx<'_>, owner: ObjId) -> NativeResult<NativeStep> {
    let size = extensions::layer_image_size(cx, Value::Obj(owner.into()))?;
    Drawing {
        owner,
        values: Vec::new(),
        size,
    }
    .next(cx)
}
impl Drawing {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let i = self.values.len();
        if i == 7 {
            return self.draw(cx);
        }
        let fallback = match i {
            0 => f64::from(self.size.width) / 2.,
            1 => f64::from(self.size.height) / 2.,
            4 | 5 => 100.,
            _ => 0.,
        };
        field(
            cx,
            Value::Obj(self.owner.into()),
            ["afx", "afy", "left", "top", "zoomx", "zoomy", "rotate"][i],
            Value::Real(fallback),
            self,
            |mut s, cx, v| {
                s.values.push(value::to_real(cx.heap(), v)?);
                s.next(cx)
            },
        )
    }
    fn draw(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let budget = budget(cx)?;
        let p = &self.values;
        let (sin, cos) = p[6].to_radians().sin_cos();
        let sx = p[4] / 100.;
        let sy = p[5] / 100.;
        let apply = |x: f64, y: f64| {
            let x = (x - p[0]) * sx;
            let y = (y - p[1]) * sy;
            [
                x * cos + y * sin + p[0] + p[2],
                y * cos - x * sin + p[1] + p[3],
            ]
        };
        let (image, mode, batch) = state(cx, self.owner, |s| {
            let Some(model) = &s.model else {
                return Err(NativeError::Message("particle manager is not initialized"));
            };
            let bytes = model
                .count
                .checked_mul(std::mem::size_of::<Sprite>())
                .and_then(|n| n.checked_add(s.old.capacity() * std::mem::size_of::<Rect>()))
                .ok_or(NativeError::Message("particle drawing size overflow"))?;
            let permit = budget.reserve(bytes).map_err(error)?;
            let old_permit = budget
                .reserve(model.count * std::mem::size_of::<Rect>())
                .map_err(error)?;
            let mut sprites = Vec::with_capacity(model.count);
            let mut bounds = Vec::with_capacity(model.count);
            for particle in model.particles.iter().rev().filter(|p| p.age >= 0) {
                let Some(&[left, top, width, height]) = s.frames.get(particle.image) else {
                    continue;
                };
                if width <= 0 || height <= 0 {
                    continue;
                }
                let (sn, cs) = particle.angle.sin_cos();
                let scale = particle.magnify;
                let point = |x: f64, y: f64| {
                    apply(
                        (x * cs + y * sn) * scale + particle.x,
                        (y * cs - x * sn) * scale + particle.y,
                    )
                };
                let w = f64::from(width) / 2.;
                let h = f64::from(height) / 2.;
                let corners = [point(-w, -h), point(w, -h), point(w, h), point(-w, h)];
                if corners.iter().flatten().any(|v| !v.is_finite()) {
                    return Err(NativeError::Message("particle transform must be finite"));
                }
                let minx = corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
                let miny = corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
                let maxx = corners
                    .iter()
                    .map(|p| p[0])
                    .fold(f64::NEG_INFINITY, f64::max);
                let maxy = corners
                    .iter()
                    .map(|p| p[1])
                    .fold(f64::NEG_INFINITY, f64::max);
                let rect = Rect {
                    left: minx as i32,
                    top: miny as i32,
                    width: (maxx - minx + 2.).max(0.) as u32,
                    height: (maxy - miny + 2.).max(0.) as u32,
                };
                if let Some(rect) = rect.intersection(self.size.rect()) {
                    bounds.push(rect);
                }
                // The DLL passes three corners minus half a pixel to
                // Layer.operateAffine, with its default nearest sampling.
                let points = [corners[0], corners[1], corners[3]].map(|p| [p[0] - 0.5, p[1] - 0.5]);
                sprites.push(Sprite {
                    source: Rect {
                        left,
                        top,
                        width: width as u32,
                        height: height as u32,
                    },
                    points,
                    opacity: (particle.opacity as i32).clamp(0, 255) as u8,
                });
            }
            let clear = std::mem::replace(&mut s.old, bounds);
            s.old_permit = Some(old_permit);
            Ok((
                s.image,
                s.mode,
                Arc::new(Sprites {
                    clear,
                    sprites,
                    _permit: permit,
                }),
            ))
        })?;
        if matches!(image,Value::Obj(o) if o.object.is_none()) {
            return Ok(NativeStep::Return(Value::Void));
        }
        extensions::layer_draw_sprites(cx, Value::Obj(self.owner.into()), image, batch, mode)
    }
}
