use super::{image::Image, layer, render};
use krkr_engine::{
    extensions,
    protocol::{budget::Budget, pixels::Pixels},
};
use std::sync::Arc;
use tjs_bind::Utf16;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, value};
pub fn redraw(
    cx: &mut NativeCx<'_>,
    owner: tjs_core::ObjId,
    result: Value,
) -> NativeResult<NativeStep> {
    if !layer::state(cx, owner, |s| s.record.is_some())? {
        return Ok(NativeStep::Return(result));
    }
    // SDL's redraw clears and draws only a bitmap surface. A vector recording
    // has no surface; path replay is provided by drawImageAffine instead.
    render::batch(
        cx,
        Value::Obj(owner.into()),
        render::Batch {
            drawings: Vec::new(),
            clear: Some(0),
            update: true,
            whole: true,
        },
        result,
    )
}
#[tjs_bind::function(resumable = true)]
pub(super) fn clear(cx: &mut NativeCx<'_>, color: Value) -> NativeResult<NativeStep> {
    let color = value::to_integer(cx.heap(), color)? as u32;
    let owner = cx.this();
    let size = extensions::layer_size(cx, Value::Obj(owner.into()))?;
    layer::state(cx, owner, |s| {
        if s.record.is_some() {
            let mut image = Image::vector(size);
            image.background = color;
            s.record = Some(image);
        }
    })?;
    render::batch(
        cx,
        Value::Obj(owner.into()),
        render::Batch {
            drawings: Vec::new(),
            clear: Some(color),
            update: true,
            whole: true,
        },
        Value::Void,
    )
}
#[tjs_bind::function]
fn get(cx: &mut NativeCx<'_>) -> NativeResult<bool> {
    layer::state(cx, cx.this(), |s| s.record.is_some())
}
#[tjs_bind::function]
fn set(cx: &mut NativeCx<'_>, enabled: bool) -> NativeResult<()> {
    let owner = cx.this();
    let size = extensions::layer_size(cx, Value::Obj(owner.into()))?;
    layer::state(cx, owner, |s| {
        if enabled {
            if s.record.is_none() {
                s.record = Some(Image::vector(size));
            }
        } else {
            s.record = None;
        }
    })
}
pub(super) static PROPERTY: tjs_core::NativeProperty = tjs_core::NativeProperty {
    name: "record",
    doc: "LayerExDraw vector recording",
    hidden: false,
    class_only: false,
    get: Some(get::CALL),
    set: Some(set::CALL),
};
#[tjs_bind::function(resumable = true)]
pub(super) fn image(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let image = layer::state(cx, owner, |s| s.record.as_ref().map(Image::duplicate))?;
    let Some(image) = image else {
        return Ok(NativeStep::Return(Value::Void));
    };
    let result = super::image::make(cx, image)?;
    redraw(cx, owner, result)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn redraw_record(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let enabled = layer::state(cx, owner, |s| s.record.is_some())?;
    redraw(cx, owner, Value::Int(i64::from(enabled)))
}
#[tjs_bind::function]
pub(super) fn load(cx: &mut NativeCx<'_>, _name: Utf16) -> NativeResult<bool> {
    layer::state(cx, cx.this(), |_| ())?;
    // krkrsdl3 loadRecord explicitly returns false without reading the filename.
    Ok(false)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn save(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
    let owner = cx.this();
    layer::state(cx, owner, |_| ())?;
    let budget = extensions::layer_pixel_budget(cx, Value::Obj(owner.into()))?;
    extensions::layer_read_pixels(
        cx,
        Value::Obj(owner.into()),
        Box::new(Save {
            owner,
            name: name.0,
            budget,
        }),
    )
}
struct Save {
    owner: tjs_core::ObjId,
    name: Vec<u16>,
    budget: Budget,
}
impl Trace for Save {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.owner.trace(v);
    }
}
impl extensions::PixelContinuation for Save {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        let Self {
            owner,
            name,
            budget,
        } = *self;
        let path = krkr_engine::assets::local::path(&name)
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        let limit = krkr_engine::storages::service(cx)?
            .borrow()
            .limits()
            .max_read_bytes as u64;
        let target = krkr_engine::assets::WritePlan::local(path, limit)
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        let request = krkr_image::export::Request {
            pixels,
            target: Some(target),
            format: krkr_image::export::Format::Png {
                rgba: true,
                unfiltered: false,
            },
            options: Default::default(),
            budget,
        };
        extensions::run_work(
            cx,
            move |stop| {
                Ok(request
                    .run(stop, &std::sync::atomic::AtomicU8::new(0))
                    .is_ok())
            },
            Box::new(Saved(owner)),
        )
    }
}
#[derive(tjs_bind::Trace)]
struct Saved(tjs_core::ObjId);
impl extensions::WorkContinuation<bool> for Saved {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, saved: bool) -> NativeResult<NativeStep> {
        if saved {
            krkr_engine::storages::service(cx)?
                .borrow_mut()
                .clear_archive_cache();
        }
        redraw(cx, self.0, Value::Int(i64::from(saved)))
    }
}
