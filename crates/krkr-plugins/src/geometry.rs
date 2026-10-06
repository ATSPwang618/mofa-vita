//! ksupport geometry, following kwidgets rect.cpp (e7bed32).
use crate::exports::{Exports, arg, object};
use krkr_engine::plugins::Context;
use tjs_bind::RestArgs;
use tjs_core::{NativeCx, NativeError, NativeResult, ObjId, Value, value};

#[derive(Clone, Copy, Default, tjs_bind::Trace)]
struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}
impl Rect {
    fn new(left: f64, top: f64, width: f64, height: f64) -> Self {
        Self {
            left,
            top,
            right: width + left,
            bottom: height + top,
        }
    }
    fn dup(self) -> Self {
        Self::new(
            self.left,
            self.top,
            self.right - self.left,
            self.bottom - self.top,
        )
    }
    fn valid(self) -> bool {
        self.left <= self.right && self.top <= self.bottom
    }
    fn intersects(self, b: Self) -> bool {
        !(b.right <= self.left
            || self.right <= b.left
            || b.bottom <= self.top
            || self.bottom <= b.top)
    }
    fn contains(self, x: f64, y: f64) -> bool {
        self.left <= x && x < self.right && self.top <= y && y <= self.bottom
    }
    // C++ uses ordered ternaries, not f64::min/max: operand order matters for
    // NaN and signed zero, and touching edges remain valid intersections.
    fn intersection(self, b: Self) -> Self {
        Self {
            left: if self.left > b.left {
                self.left
            } else {
                b.left
            },
            top: if self.top > b.top { self.top } else { b.top },
            right: if self.right < b.right {
                self.right
            } else {
                b.right
            },
            bottom: if self.bottom < b.bottom {
                self.bottom
            } else {
                b.bottom
            },
        }
    }
    fn union(self, b: Self) -> Self {
        Self {
            left: if self.left < b.left {
                self.left
            } else {
                b.left
            },
            top: if self.top < b.top { self.top } else { b.top },
            right: if self.right > b.right {
                self.right
            } else {
                b.right
            },
            bottom: if self.bottom > b.bottom {
                self.bottom
            } else {
                b.bottom
            },
        }
    }
}
fn read_rect(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Rect> {
    cx.heap_mut()
        .with_native_state::<rectangle::State, _>(object(value)?, |s| s.get().copied())?
}
fn make_rect(cx: &mut NativeCx<'_>, rect: Rect) -> NativeResult<Value> {
    let class = cx
        .heap()
        .registered_class("KRect")
        .expect("installed KRect");
    Ok(Value::Obj(tjs_core::ObjRef::bound(
        cx.heap_mut()
            .alloc_native(class, rectangle::State { rect: Some(rect) })?,
    )))
}
#[tjs_bind::class(name = "KRect")]
mod rectangle {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub(super) rect: Option<Rect>,
    }
    impl State {
        pub(super) fn get(&self) -> NativeResult<&Rect> {
            self.rect.as_ref().ok_or(NativeError::This)
        }
        fn get_mut(&mut self) -> NativeResult<&mut Rect> {
            self.rect.as_mut().ok_or(NativeError::This)
        }
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Self> {
            // ncbind's single-void constructor creates the script shell only.
            if matches!(args, [Value::Void]) {
                return Ok(Self::default());
            }
            if args.len() < 4 {
                return Err(NativeError::Missing(args.len()));
            }
            let left = value::to_real(cx.heap(), arg(args, 0)?)?;
            let top = value::to_real(cx.heap(), arg(args, 1)?)?;
            let width = value::to_real(cx.heap(), arg(args, 2)?)?;
            let height = value::to_real(cx.heap(), arg(args, 3)?)?;
            Ok(Self {
                rect: Some(Rect::new(left, top, width, height)),
            })
        }
        #[tjs::getter(name = "isValid")]
        fn valid(&self) -> NativeResult<bool> {
            Ok(self.get()?.valid())
        }
        #[tjs::getter]
        fn left(&self) -> NativeResult<f64> {
            Ok(self.get()?.left)
        }
        #[tjs::setter(name = "left")]
        fn set_left(&mut self, v: f64) -> NativeResult<()> {
            self.get_mut()?.left = v;
            Ok(())
        }
        #[tjs::getter]
        fn top(&self) -> NativeResult<f64> {
            Ok(self.get()?.top)
        }
        #[tjs::setter(name = "top")]
        fn set_top(&mut self, v: f64) -> NativeResult<()> {
            self.get_mut()?.top = v;
            Ok(())
        }
        #[tjs::getter]
        fn right(&self) -> NativeResult<f64> {
            Ok(self.get()?.right)
        }
        #[tjs::setter(name = "right")]
        fn set_right(&mut self, v: f64) -> NativeResult<()> {
            self.get_mut()?.right = v;
            Ok(())
        }
        #[tjs::getter]
        fn bottom(&self) -> NativeResult<f64> {
            Ok(self.get()?.bottom)
        }
        #[tjs::setter(name = "bottom")]
        fn set_bottom(&mut self, v: f64) -> NativeResult<()> {
            self.get_mut()?.bottom = v;
            Ok(())
        }
        #[tjs::getter]
        fn width(&self) -> NativeResult<f64> {
            let r = self.get()?;
            Ok(r.right - r.left)
        }
        #[tjs::setter(name = "width")]
        fn set_width(&mut self, v: f64) -> NativeResult<()> {
            let r = self.get_mut()?;
            r.right = r.left + v;
            Ok(())
        }
        #[tjs::getter]
        fn height(&self) -> NativeResult<f64> {
            let r = self.get()?;
            Ok(r.bottom - r.top)
        }
        #[tjs::setter(name = "height")]
        fn set_height(&mut self, v: f64) -> NativeResult<()> {
            let r = self.get_mut()?;
            r.bottom = r.top + v;
            Ok(())
        }
        #[tjs::method]
        fn contains(&self, x: f64, y: f64) -> NativeResult<bool> {
            Ok(self.get()?.contains(x, y))
        }
        #[tjs::method]
        fn dup(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            make_rect(cx, self.get()?.dup())
        }
        #[tjs::method]
        fn assign(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 0)
        }
        #[tjs::method]
        fn include(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 1)
        }
        #[tjs::method(name = "unionWith")]
        fn union(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 1)
        }
        #[tjs::method(name = "intersectWith")]
        fn intersect(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 2)
        }
        #[tjs::method]
        fn intersects(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<bool> {
            let rect = read_rect(cx, value)?;
            let this = cx.this();
            cx.heap_mut()
                .with_native_state::<State, _>(this, |s| Ok(s.get()?.intersects(rect)))?
        }
    }
    fn change(cx: &mut NativeCx<'_>, value: Value, mode: u8) -> NativeResult<()> {
        let rect = read_rect(cx, value)?;
        let this = cx.this();
        cx.heap_mut().with_native_state::<State, _>(this, |s| {
            let current = s.get_mut()?;
            *current = match mode {
                0 => rect,
                1 => current.union(rect),
                _ => current.intersection(rect),
            };
            Ok(())
        })?
    }
}
enum Input {
    Rect(Rect),
    Region(Vec<Rect>),
}
fn rects(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Input> {
    let invalid = || NativeError::Message("invalid argumen for function");
    if matches!(value,Value::Obj(reference) if reference.object.is_none()) {
        return Err(invalid());
    }
    let id = object(value)?;
    if let Ok(rect) = read_rect(cx, value) {
        return Ok(Input::Rect(rect));
    }
    match cx
        .heap_mut()
        .with_native_state::<region::State, _>(id, |s| s.get().cloned())
    {
        Ok(Ok(rects)) => Ok(Input::Region(rects)),
        _ => Err(invalid()),
    }
}
fn push_rect(out: &mut Vec<Rect>, rect: Rect) -> NativeResult<()> {
    if out.len() >= 65536 {
        return Err(NativeError::Message("region exceeds rectangle limit"));
    }
    out.try_reserve(1)
        .map_err(|_| NativeError::Message("region allocation failed"))?;
    out.push(rect);
    Ok(())
}
fn subtract(rects: &[Rect], cut: Rect) -> NativeResult<Vec<Rect>> {
    let mut out = Vec::new();
    for &rect in rects {
        let mut r = rect;
        if !r.intersects(cut) {
            push_rect(&mut out, r)?;
            continue;
        }
        if cut.top > r.top {
            push_rect(
                &mut out,
                Rect::new(r.left, r.top, r.right - r.left, cut.top - r.top),
            )?;
            r.top = cut.top;
        }
        if cut.bottom < r.bottom {
            push_rect(
                &mut out,
                Rect::new(r.left, cut.bottom, r.right - r.left, r.bottom - cut.bottom),
            )?;
            r.bottom = cut.bottom;
        }
        if cut.left > r.left {
            push_rect(
                &mut out,
                Rect::new(r.left, r.top, cut.left - r.left, r.bottom - r.top),
            )?;
        }
        if cut.right < r.right {
            push_rect(
                &mut out,
                Rect::new(cut.right, r.top, r.right - cut.right, r.bottom - r.top),
            )?;
        }
    }
    Ok(out)
}
#[tjs_bind::class(name = "KRegion")]
mod region {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        rects: Option<Vec<Rect>>,
    }
    impl State {
        pub(super) fn get(&self) -> NativeResult<&Vec<Rect>> {
            self.rects.as_ref().ok_or(NativeError::This)
        }
        fn get_mut(&mut self) -> NativeResult<&mut Vec<Rect>> {
            self.rects.as_mut().ok_or(NativeError::This)
        }
        #[tjs::constructor]
        fn new(args: RestArgs<'_>) -> Self {
            Self {
                rects: if matches!(args, [Value::Void]) {
                    None
                } else {
                    Some(Vec::new())
                },
            }
        }
        #[tjs::getter(name = "rectCount")]
        fn count(&self) -> NativeResult<i64> {
            Ok(self.get()?.len() as i64)
        }
        #[tjs::getter]
        fn empty(&self) -> NativeResult<bool> {
            Ok(self.get()?.is_empty())
        }
        #[tjs::method]
        fn clear(&mut self) -> NativeResult<()> {
            self.get_mut()?.clear();
            Ok(())
        }
        #[tjs::method(name = "rectAt")]
        fn at(&self, cx: &mut NativeCx<'_>, index: i64) -> NativeResult<Value> {
            // Source indexes an unchecked vector with tjs_uint (32 bit). Keep
            // narrowing, but report an error instead of reproducing C++ UB.
            let rect = self
                .get()?
                .get(index as u32 as usize)
                .ok_or(NativeError::Message("region rectangle index out of range"))?;
            make_rect(cx, rect.dup())
        }
        #[tjs::method]
        fn contains(&self, x: f64, y: f64) -> NativeResult<bool> {
            Ok(self.get()?.iter().any(|r| r.contains(x, y)))
        }
        #[tjs::method]
        fn intersects(cx: &mut NativeCx<'_>, rect: Value) -> NativeResult<bool> {
            let rect = read_rect(cx, rect)?;
            let this = cx.this();
            cx.heap_mut().with_native_state::<State, _>(this, |s| {
                Ok(s.get()?.iter().any(|r| r.intersects(rect)))
            })?
        }
        #[tjs::method]
        fn assign(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 0)
        }
        #[tjs::method]
        fn include(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 1)
        }
        #[tjs::method]
        fn exclude(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 2)
        }
        #[tjs::method(name = "intersectWith")]
        fn intersect(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            change(cx, value, 3)
        }
    }
    fn change(cx: &mut NativeCx<'_>, value: Value, mode: u8) -> NativeResult<()> {
        let input = rects(cx, value)?;
        let this = cx.this();
        cx.heap_mut().with_native_state::<State, _>(this, |s| {
            let current = s.get_mut()?;
            let (input, is_rect) = match input {
                Input::Rect(r) => (vec![r], true),
                Input::Region(rs) => (rs, false),
            };
            let output = match mode {
                0 => input,
                1 => {
                    let mut addition = input;
                    for &cut in current.iter() {
                        addition = subtract(&addition, cut)?;
                    }
                    let mut out = current.clone();
                    for rect in addition {
                        push_rect(&mut out, rect)?;
                    }
                    out
                }
                2 => {
                    let mut out = current.clone();
                    for cut in input {
                        out = subtract(&out, cut)?;
                    }
                    out
                }
                _ => {
                    let mut out = Vec::new();
                    for a in input {
                        for &b in current.iter() {
                            let rect = if is_rect {
                                b.intersection(a)
                            } else {
                                a.intersection(b)
                            };
                            if rect.valid() {
                                push_rect(&mut out, rect)?;
                            }
                        }
                    }
                    out
                }
            };
            if output.len() > 65536 {
                return Err(NativeError::Message("region exceeds rectangle limit"));
            }
            *current = output;
            Ok(())
        })?
    }
}
pub(crate) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let classes: [(&str, ObjId); 2] = [
        ("KRect", rectangle::install(cx.heap)?),
        ("KRegion", region::install(cx.heap)?),
    ];
    for (name, class) in classes {
        exports.value(cx, cx.global, name, Value::Obj(class.into()))?;
    }
    Ok(())
}
