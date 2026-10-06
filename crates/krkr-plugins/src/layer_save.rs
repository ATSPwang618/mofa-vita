//! layerExSave: portable image exports, Window jobs, and the full utility set.
//! Original author Go Watanabe; see layer_save/LICENSE.txt.
mod metadata;
mod pixels;
mod windows;
use krkr_engine::{
    extensions,
    plugins::{Context, Exports, Plugin},
};
use krkr_image::export::{Format, Options, Request, Service};
use std::{cell::Cell, rc::Rc, sync::Arc};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Trace, Value,
    value,
};

#[derive(Clone, Default)]
struct Shared {
    busy: Rc<Cell<usize>>,
    service: Service,
}
impl Trace for Shared {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
struct Lease(Shared);
impl Trace for Lease {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.busy.set(self.0.busy.get() - 1);
    }
}
impl Shared {
    fn lease(&self) -> Lease {
        self.busy.set(self.busy.get() + 1);
        Lease(self.clone())
    }
}
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Save {
    exports: Exports,
    shared: Shared,
}
krkr_engine::native_plugin! {impl Save {names:["layerExSave.dll","layerExSave.tpm"]}}
impl Plugin for Save {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let layer = crate::exports::class(cx, "Layer")?;
        let window = crate::exports::class(cx, "Window")?;
        cx.heap
            .register_state_invalidator::<windows::State>(windows::cleanup::CALL);
        let mut exports = Exports::default();
        for (name, call) in [
            ("saveLayerImageTlg5", tlg5::CALL),
            ("saveLayerImagePng", png::CALL),
            ("saveLayerImagePngOctet", octet::CALL),
            ("getCropRect", pixels::crop::CALL),
            ("getCropRectZero", pixels::zero::CALL),
            ("getDiffRect", pixels::diff_rect::CALL),
            ("getDiffPixel", pixels::diff_pixel::CALL),
            ("oozeColor", pixels::ooze::CALL),
            ("copyBlueToAlpha", pixels::blue::CALL),
            ("isBlank", pixels::blank::CALL),
            ("clearAlpha", pixels::clear::CALL),
            ("getAverageColor", pixels::average::CALL),
        ] {
            exports.captured_function(cx, layer, name, call, self.shared.clone(), false)?;
        }
        for (name, call) in [
            ("startSaveLayerImage", windows::start::CALL),
            ("cancelSaveLayerImage", windows::cancel::CALL),
            ("stopSaveLayerImage", windows::stop::CALL),
        ] {
            exports.captured_function(cx, window, name, call, self.shared.clone(), false)?;
        }
        self.exports = exports;
        Ok(())
    }
    fn can_unlink(&self, _: &Context<'_>) -> NativeResult<bool> {
        Ok(self.shared.busy.get() == 0)
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        if !self.can_unlink(cx)? {
            Ok(false)
        } else {
            self.exports.unlink(cx)
        }
    }
}
fn capture(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let function = cx.function().ok_or(NativeError::This)?;
    cx.heap_mut()
        .with_native_state::<Shared, _>(function, |s| s.clone())
}
fn detail(error: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(error.to_string())
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
fn text(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), v)? else {
        unreachable!()
    };
    Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
}
fn nullable(v: Value) -> NativeResult<Option<Value>> {
    match v {
        Value::Void => Ok(None),
        Value::Obj(r) => Ok(r.object.map(|_| v)),
        _ => Err(NativeError::Type("an object")),
    }
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Mode {
    Tlg,
    Png,
    Octet,
    BackgroundPng,
    BackgroundTlg,
}
impl Mode {
    fn tlg(self) -> bool {
        matches!(self, Self::Tlg | Self::BackgroundTlg)
    }
}
macro_rules! save_method {
    ($name:ident,$mode:ident) => {
        #[tjs_bind::function(resumable = true)]
        fn $name(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
            save(cx, args, Mode::$mode)
        }
    };
}
save_method!(tlg5, Tlg);
save_method!(png, Png);
save_method!(octet, Octet);
fn save(cx: &mut NativeCx<'_>, args: &[Value], mode: Mode) -> NativeResult<NativeStep> {
    if matches!(mode, Mode::Octet) && !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    let name = if matches!(mode, Mode::Octet) {
        None
    } else {
        Some(text(cx, crate::exports::arg(args, 0)?)?)
    };
    let tags = if matches!(mode, Mode::Octet) {
        args.first().copied()
    } else {
        args.get(1).copied()
    };
    if !matches!(mode, Mode::Octet) {
        nullable(tags.unwrap_or(Value::Void))?;
    }
    let shared = capture(cx)?;
    let owner = Value::Obj(cx.this().into());
    pixels::read(
        cx,
        owner,
        Box::new(Encoding {
            owner,
            name,
            tags,
            mode,
            lease: shared.lease(),
            background: None,
        }),
    )
}
#[derive(tjs_bind::Trace)]
struct Encoding {
    owner: Value,
    name: Option<Vec<u16>>,
    tags: Option<Value>,
    mode: Mode,
    lease: Lease,
    background: Option<windows::Pending>,
}
impl extensions::PixelContinuation for Encoding {
    fn pixels(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_engine::protocol::pixels::Pixels>,
    ) -> NativeResult<NativeStep> {
        metadata::begin(cx, *self, pixels)
    }
}
#[derive(tjs_bind::Trace)]
struct Saved {
    lease: Lease,
    #[trace(skip = "Resolved host filename contains no script handles")]
    path: Option<std::path::PathBuf>,
}
impl Encoding {
    fn encode(
        self,
        cx: &mut NativeCx<'_>,
        pixels: Arc<krkr_engine::protocol::pixels::Pixels>,
        options: Options,
    ) -> NativeResult<NativeStep> {
        let storage = krkr_engine::storages::service(cx)?;
        let target = self
            .name
            .as_ref()
            .map(|name| storage.borrow().write_plan(name).map_err(detail))
            .transpose()?;
        let path = target.as_ref().map(|target| target.path().to_owned());
        let request = Request {
            pixels,
            target,
            format: if self.mode.tlg() {
                Format::Tlg5
            } else {
                Format::Png {
                    rgba: matches!(self.mode, Mode::BackgroundPng),
                    unfiltered: matches!(self.mode, Mode::BackgroundPng),
                }
            },
            options,
            budget: extensions::layer_pixel_budget(cx, self.owner)?,
        };
        if let Some(background) = self.background {
            windows::validate(cx, &background)?;
            let job = self.lease.0.service.submit(request).map_err(detail)?;
            return windows::started(cx, background, self.lease, job, path);
        }
        let step = extensions::encode_image(cx, request)?;
        Ok(tjs_bind::flow::then(
            step,
            tjs_bind::flow::callback(
                Saved {
                    lease: self.lease,
                    path,
                },
                |saved, cx, value| {
                    if let Some(path) = saved.path {
                        krkr_engine::storages::service(cx)?
                            .borrow_mut()
                            .invalidate_file(&path);
                    }
                    Ok(NativeStep::Return(value))
                },
            ),
        ))
    }
}
