//! Original layerExSave pixel rules, including its blue-only blank predicate
//! and byte-reversed average result. Work yields by row; no script buffer ABI.
use super::*;
use krkr_engine::protocol::{
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::sync::Arc;

#[derive(Clone, Copy, tjs_bind::Trace)]
enum Kind {
    Crop,
    Zero,
    DiffRect,
    DiffPixel,
    Ooze,
    Blue,
    Blank,
    Clear,
    Average,
}
macro_rules! method {
    ($name:ident,$kind:ident) => {
        #[tjs_bind::function(resumable = true)]
        pub(super) fn $name(
            cx: &mut NativeCx<'_>,
            args: tjs_bind::RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            begin(cx, args, Kind::$kind)
        }
    };
}
method!(crop, Crop);
method!(zero, Zero);
method!(diff_rect, DiffRect);
method!(diff_pixel, DiffPixel);
method!(ooze, Ooze);
method!(blue, Blue);
method!(blank, Blank);
method!(clear, Clear);
method!(average, Average);

#[derive(tjs_bind::Trace)]
struct Work {
    lease: Option<Lease>,
    owner: Value,
    source: Value,
    kind: Kind,
    args: Vec<Value>,
    colors: [Option<u32>; 2],
    level: i32,
    threshold: i32,
    #[trace(skip = "Budgeted CPU readback has no script values")]
    input: Option<Arc<Pixels>>,
    #[trace(skip = "Budgeted CPU readback has no script values")]
    base: Option<Arc<Pixels>>,
    #[trace(skip = "Owned budgeted output and diffusion map have no script values")]
    output: Option<Bytes>,
    #[trace(skip = "Owned budgeted diffusion map has no script values")]
    map: Option<Bytes>,
    rect: [i64; 4],
    bounds: [u32; 4],
    sums: [u64; 4],
    count: u64,
    row: usize,
    pass: i32,
    promoting: bool,
    changed: bool,
}
fn begin(cx: &mut NativeCx<'_>, args: &[Value], kind: Kind) -> NativeResult<NativeStep> {
    let required = match kind {
        Kind::DiffRect | Kind::DiffPixel | Kind::Ooze | Kind::Blue => 1,
        Kind::Blank | Kind::Average => 4,
        _ => 0,
    };
    if args.len() < required {
        return Err(NativeError::Missing(required - 1));
    }
    let mut w = Box::new(Work {
        lease: Some(capture(cx)?.lease()),
        owner: Value::Obj(cx.this().into()),
        source: Value::Void,
        kind,
        args: args.iter().copied().take(4).collect(),
        colors: [None; 2],
        level: 0,
        threshold: 0,
        input: None,
        base: None,
        output: None,
        map: None,
        rect: [0; 4],
        bounds: [u32::MAX, u32::MAX, 0, 0],
        sums: [0; 4],
        count: 0,
        row: 0,
        pass: -1,
        promoting: false,
        changed: false,
    });
    if matches!(kind, Kind::DiffPixel) {
        for (i, color) in w.colors.iter_mut().enumerate() {
            if let Some(v) = args.get(i + 1).filter(|v| !matches!(v, Value::Void)) {
                *color = Some(value::to_integer(cx.heap(), *v)? as u32);
            }
        }
    }
    if matches!(kind, Kind::Ooze | Kind::Clear) {
        let number = |i: usize, default: i64| -> NativeResult<i32> {
            Ok(args
                .get(i)
                .map(|&v| value::to_integer(cx.heap(), v))
                .transpose()?
                .unwrap_or(default) as i32)
        };
        if matches!(kind, Kind::Ooze) {
            w.level = number(0, 0)?;
            if w.level <= 0 {
                return Err(NativeError::Message("invalid level count"));
            }
            w.threshold = (number(1, 1)? as u8).max(1) as i32;
            w.colors[0] = Some(number(2, 0)? as u32 & 0xffffff);
        } else {
            w.threshold = number(0, 0)?;
            w.colors[0] = Some(number(1, 0)? as u32 & 0xffffff);
        }
    }
    if matches!(kind, Kind::DiffRect | Kind::DiffPixel | Kind::Blue) {
        crate::exports::object(args[0])?;
        w.source = args[0];
    }
    let first = if matches!(kind, Kind::Blue) {
        w.source
    } else {
        w.owner
    };
    read(cx, first, w)
}
impl extensions::PixelContinuation for Work {
    fn pixels(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        if matches!(self.kind, Kind::Blue) && self.base.is_none() {
            self.base = Some(pixels);
            return read(cx, self.owner, self);
        }
        if self.input.is_none() {
            self.input = Some(pixels);
            if matches!(self.kind, Kind::DiffRect | Kind::DiffPixel) {
                return read(cx, self.source, self);
            }
        } else {
            self.base = Some(pixels);
        }
        let input = self.input.as_ref().unwrap();
        let size = input.size;
        if matches!(self.kind, Kind::DiffRect | Kind::DiffPixel)
            && self.base.as_ref().unwrap().size != size
        {
            return Err(NativeError::Message("different layer size"));
        }
        if matches!(self.kind, Kind::Blank | Kind::Average) {
            for (i, v) in self.args.iter().enumerate() {
                self.rect[i] = i64::from(value::to_integer(cx.heap(), *v)? as i32);
            }
            let [x, y, w, h] = self.rect;
            let right = (x + w).min(i64::from(size.width));
            let bottom = (y + h).min(i64::from(size.height));
            self.rect = [x.max(0), y.max(0), right, bottom];
            if matches!(self.kind, Kind::Average) && (right <= x.max(0) || bottom <= y.max(0)) {
                return Err(NativeError::Message("invalid layer range"));
            }
        }
        if matches!(
            self.kind,
            Kind::Ooze | Kind::Blue | Kind::Clear | Kind::DiffPixel
        ) {
            extensions::layer_prepare_draw(cx, self.owner)?;
            let budget = extensions::layer_pixel_budget(cx, self.owner)?;
            self.output = Some(Bytes::zeroed(size.rgba_bytes().unwrap(), &budget).map_err(detail)?);
            if matches!(self.kind, Kind::Ooze) {
                self.map = Some(
                    Bytes::zeroed(size.width as usize * size.height as usize, &budget)
                        .map_err(detail)?,
                );
            }
        }
        self.resume(cx, Value::Void)
    }
}
fn same(a: &[u8], b: &[u8]) -> bool {
    a[3] == b[3] && (a[3] == 0 || a[..3] == b[..3])
}
fn rgba(c: u32) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, (c >> 24) as u8]
}
impl NativeContinuation for Work {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let size = self.input.as_ref().unwrap().size;
        let w = size.width as usize;
        let h = size.height as usize;
        if self.promoting {
            let start = self.row * w;
            for v in &mut self.map.as_mut().unwrap().as_mut_slice()[start..start + w] {
                if *v == 1 {
                    *v = 255;
                }
            }
            self.row += 1;
            if self.row == h {
                self.row = 0;
                self.promoting = false;
                self.pass += 1;
                self.changed = false;
            }
            return Ok(NativeStep::Continue(self));
        }
        if self.row < h {
            let y = self.row;
            let input = self
                .input
                .as_ref()
                .unwrap()
                .main
                .as_ref()
                .unwrap()
                .as_slice();
            let base = self
                .base
                .as_ref()
                .map(|v| v.main.as_ref().unwrap().as_slice());
            for x in 0..w {
                let i = y * w + x;
                let p = &input[i * 4..i * 4 + 4];
                if self.pass < 0 {
                    if let Some(output) = &mut self.output {
                        output.as_mut_slice()[i * 4..i * 4 + 4].copy_from_slice(p);
                    }
                    let found = match self.kind {
                        Kind::Crop => p[3] != 0,
                        Kind::Zero => p.iter().any(|&v| v != 0),
                        Kind::DiffRect | Kind::DiffPixel => {
                            !same(p, &base.unwrap()[i * 4..i * 4 + 4])
                        }
                        _ => false,
                    };
                    if found {
                        self.bounds[0] = self.bounds[0].min(x as u32);
                        self.bounds[1] = self.bounds[1].min(y as u32);
                        self.bounds[2] = self.bounds[2].max(x as u32);
                        self.bounds[3] = self.bounds[3].max(y as u32);
                        self.count += 1;
                    }
                    let replacement = match self.kind {
                        Kind::DiffPixel => self.colors[usize::from(found)],
                        Kind::Clear if i32::from(p[3]) <= self.threshold => self.colors[0],
                        _ => None,
                    };
                    if let Some(c) = replacement {
                        self.output.as_mut().unwrap().as_mut_slice()[i * 4..i * 4 + 4]
                            .copy_from_slice(&rgba(c));
                    }
                    match self.kind {
                        Kind::Blue => {
                            let b = self.base.as_ref().unwrap();
                            if x < b.size.width as usize && y < b.size.height as usize {
                                self.output.as_mut().unwrap().as_mut_slice()[i * 4 + 3] =
                                    base.unwrap()[(y * b.size.width as usize + x) * 4 + 2];
                            }
                        }
                        Kind::Ooze => {
                            if i32::from(p[3]) >= self.threshold {
                                self.map.as_mut().unwrap().as_mut_slice()[i] = 255;
                            } else {
                                self.output.as_mut().unwrap().as_mut_slice()[i * 4..i * 4 + 3]
                                    .copy_from_slice(&rgba(self.colors[0].unwrap())[..3]);
                            }
                        }
                        Kind::Blank | Kind::Average => {
                            let [l, t, r, b] = self.rect;
                            if x as i64 >= l && (x as i64) < r && y as i64 >= t && (y as i64) < b {
                                if matches!(self.kind, Kind::Blank) && p[2] != 0 {
                                    return Ok(NativeStep::Return(Value::Int(0)));
                                }
                                for (sum, v) in self.sums.iter_mut().zip(p) {
                                    *sum += u64::from(*v);
                                }
                                self.count += 1;
                            }
                        }
                        _ => {}
                    }
                } else {
                    let map = self.map.as_ref().unwrap().as_slice();
                    if map[i] != 0 {
                        continue;
                    }
                    let mut sums = [0u32; 3];
                    let mut count = 0;
                    for j in [
                        y.checked_sub(1).map(|y| y * w + x),
                        (y + 1 < h).then_some(i + w),
                        x.checked_sub(1).map(|x| y * w + x),
                        (x + 1 < w).then_some(i + 1),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if map[j] == 255 {
                            for (sum, v) in sums
                                .iter_mut()
                                .zip(&self.output.as_ref().unwrap().as_slice()[j * 4..j * 4 + 3])
                            {
                                *sum += u32::from(*v);
                            }
                            count += 1;
                        }
                    }
                    if let Some(count) = std::num::NonZeroU32::new(count) {
                        for (v, sum) in self.output.as_mut().unwrap().as_mut_slice()
                            [i * 4..i * 4 + 3]
                            .iter_mut()
                            .zip(sums)
                        {
                            *v = (sum / count.get()) as u8;
                        }
                        self.map.as_mut().unwrap().as_mut_slice()[i] = 1;
                        self.changed = true;
                    }
                }
            }
            self.row += 1;
            return Ok(NativeStep::Continue(self));
        }
        if matches!(self.kind, Kind::Ooze)
            && self.pass < self.level - 1
            && (self.pass < 0 || self.changed)
        {
            // Promote the completed frontier only after the whole pass. New
            // colors must not leak farther during the same scan.
            self.promoting = true;
            self.row = 0;
            return Ok(NativeStep::Continue(self));
        }
        let result = match self.kind {
            Kind::Crop | Kind::Zero | Kind::DiffRect => {
                if self.count == 0 {
                    Value::Void
                } else {
                    let [x, y, r, b] = self.bounds;
                    let object = cx.heap_mut().alloc_dictionary();
                    for (name, v) in
                        ["x", "y", "w", "h"]
                            .into_iter()
                            .zip([x, y, r - x + 1, b - y + 1])
                    {
                        let key = cx
                            .heap_mut()
                            .intern(&name.encode_utf16().collect::<Vec<_>>());
                        cx.heap_mut()
                            .set_member(object, key, Value::Int(i64::from(v)))?;
                    }
                    Value::Obj(object.into())
                }
            }
            Kind::Blank => Value::Int(1),
            Kind::DiffPixel => Value::Int(self.count as i64),
            Kind::Average => {
                let v = self.sums.map(|s| (s / self.count) as u32);
                Value::Int((v[2] << 24 | v[1] << 16 | v[0] << 8 | v[3]) as i32 as i64)
            }
            _ => Value::Void,
        };
        if let Some(main) = self.output.take() {
            let pixels = Arc::new(Pixels {
                size,
                main: Some(main),
                province: None,
            });
            let step = extensions::layer_copy_pixels(cx, self.owner, pixels, false, size)?;
            Ok(tjs_bind::flow::then(
                step,
                tjs_bind::flow::callback(
                    (self.lease.take().unwrap(), result),
                    |(_lease, result), _, _| Ok(NativeStep::Return(result)),
                ),
            ))
        } else {
            Ok(NativeStep::Return(result))
        }
    }
}

#[derive(tjs_bind::Trace)]
struct Read {
    owner: Value,
    next: Box<dyn extensions::PixelContinuation>,
    index: usize,
    size: [u32; 2],
}
pub(super) fn read(
    cx: &mut NativeCx<'_>,
    owner: Value,
    next: Box<dyn extensions::PixelContinuation>,
) -> NativeResult<NativeStep> {
    extensions::layer_size(cx, owner)?;
    Box::new(Read {
        owner,
        next,
        index: 0,
        size: [0; 2],
    })
    .resume(cx, Value::Void)
}
impl NativeContinuation for Read {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        if self.index > 0 {
            let n = value::to_integer(cx.heap(), v)? as i32;
            if n <= 0 {
                return Err(NativeError::Message("invalid layer image"));
            }
            if self.index > 1 {
                self.size[self.index - 2] = n as u32;
            }
        }
        if self.index < 3 {
            let name = ["hasImage", "imageWidth", "imageHeight"][self.index];
            self.index += 1;
            let key = key(cx, name);
            return Ok(NativeStep::Get {
                object: self.owner,
                key,
                continuation: self,
            });
        }
        extensions::layer_read_pixels(cx, self.owner, self)
    }
}
impl extensions::PixelContinuation for Read {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        if pixels.size
            != (Size {
                width: self.size[0],
                height: self.size[1],
            })
        {
            return Err(NativeError::Message(
                "Layer dimensions disagree with managed image",
            ));
        }
        self.next.pixels(cx, pixels)
    }
}
