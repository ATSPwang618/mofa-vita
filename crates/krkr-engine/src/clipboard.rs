//! Portable clipboard data and subscriptions. Original plugin contract:
//! krkrz/krkrz@49c4d53506edecb824cd7b2cff8d32959b1f1b70,
//! src/plugins/win32/clipboardEx/clipboardEx.cpp (actual implementation).
mod serialize;
use krkr_protocol::{budget::Budget, pixels::Pixels};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
    value,
};

pub const TJS_FORMAT: &str = "application/x-kirikiri-tjs";
pub const LAYER_FORMAT: &str = "application/x-kirikiri-layer";
pub const DATA_LIMIT: usize = 16 * 1024 * 1024;
/// One replacement, including all requested formats. Image pixels are RGBA8,
/// top-to-bottom, with a live staging/Bitmap budget permit.
#[derive(Default)]
pub struct Data {
    pub text: Option<Vec<u16>>,
    pub tjs: Option<Vec<u8>>,
    pub image: Option<Arc<Pixels>>,
    pub image_budget: Option<Budget>,
}
/// Dropping this registration requests shutdown without waiting or running TJS.
pub trait Watch {
    fn error(&self) -> Option<String> {
        None
    }
}
pub trait Host {
    fn text(&mut self) -> Result<Option<Vec<u16>>, String>;
    fn buffer(&mut self, format: &str) -> Result<Option<Vec<u8>>, String>;
    fn image(&mut self, budget: &Budget) -> Result<Option<Pixels>, String>;
    fn write(&mut self, data: Data) -> Result<(), String>;
    fn has(&mut self, format: i32) -> Result<bool, String>;
    fn watch(&mut self, changed: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Watch>, String>;
}
#[derive(Default)]
struct Service {
    host: Option<Box<dyn Host>>,
    watch: Option<Box<dyn Watch>>,
    users: usize,
    revision: Arc<AtomicU64>,
}
type Shared = Rc<RefCell<Service>>;
fn service(heap: &mut Heap) -> NativeResult<Shared> {
    let class = heap
        .registered_class("Clipboard")
        .ok_or(NativeError::This)?;
    heap.with_native_state::<implementation::State, _>(class, |s| s.host.clone())
}
fn host<T>(
    cx: &mut NativeCx<'_>,
    call: impl FnOnce(&mut dyn Host) -> Result<T, String>,
) -> NativeResult<T> {
    let shared = service(cx.heap_mut())?;
    let mut service = shared.borrow_mut();
    call(
        service
            .host
            .as_deref_mut()
            .ok_or(NativeError::Message("clipboard host is unavailable"))?,
    )
    .map_err(NativeError::Detail)
}
pub(crate) struct Subscription {
    shared: Shared,
    pub revision: u64,
    pub token: Rc<()>,
}
impl Subscription {
    pub fn error(&self) -> Option<String> {
        self.shared.borrow().watch.as_ref().and_then(|w| w.error())
    }
    pub fn current(&self) -> u64 {
        self.shared.borrow().revision.load(Ordering::Acquire)
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        let mut service = self.shared.borrow_mut();
        service.users -= 1;
        if service.users == 0 {
            service.watch = None;
        }
    }
}
pub(crate) fn subscribe(
    heap: &mut Heap,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> NativeResult<Subscription> {
    let shared = service(heap)?;
    let mut service = shared.borrow_mut();
    if service.users == 0 {
        // A stopped platform watcher can deliver a late notification. Give
        // every new registration its own counter so an old callback cannot
        // invalidate the clipboard state seen by a later subscription.
        let revision = Arc::new(AtomicU64::new(0));
        let changed_revision = revision.clone();
        service.watch = Some(
            service
                .host
                .as_deref_mut()
                .ok_or(NativeError::Message("clipboard host is unavailable"))?
                .watch(Arc::new(move || {
                    changed_revision.fetch_add(1, Ordering::AcqRel);
                    wake();
                }))
                .map_err(NativeError::Detail)?,
        );
        service.revision = revision;
    }
    service.users += 1;
    let revision = service.revision.load(Ordering::Acquire);
    drop(service);
    Ok(Subscription {
        shared,
        revision,
        token: Rc::new(()),
    })
}
#[tjs_bind::class(name = "Clipboard", static_class = true)]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) host: Shared,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            let _ = visit;
        }
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method(name = "hasFormat", class_only = true)]
        fn has(cx: &mut NativeCx<'_>, format: i64) -> NativeResult<bool> {
            if format as i32 != 1 {
                return Ok(false);
            }
            host(cx, |h| h.has(1))
        }
        #[tjs::getter(name = "asText", class_only = true)]
        fn text(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(host(cx, |h| h.text())?
                .map_or(Value::Void, |s| Value::Str(cx.heap_mut().alloc_string(s))))
        }
        #[tjs::setter(name = "asText", class_only = true)]
        fn set_text(cx: &mut NativeCx<'_>, text: Value) -> NativeResult<()> {
            let text = text_units(cx, text)?;
            host(cx, |h| {
                h.write(Data {
                    text: Some(text),
                    ..Default::default()
                })
            })
        }
    }
}
fn text_units(cx: &NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let mut text = value::to_string_units(cx.heap(), value)?;
    if text.len() > DATA_LIMIT / 2 {
        return Err(NativeError::Message("clipboard text exceeds size limit"));
    }
    text.truncate(text.iter().position(|&u| u == 0).unwrap_or(text.len()));
    Ok(text)
}
/// The plugin overrides the base text-only query for its installed lifetime.
pub fn has_format(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let format = value::to_integer(cx.heap(), arg(args)?)? as i32;
    if !matches!(format, 1..=3) {
        return Ok(Value::Int(0));
    }
    Ok(Value::Int(host(cx, |h| h.has(format))?.into()))
}
pub fn get_tjs(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
    let Some(bytes) = host(cx, |h| h.buffer(TJS_FORMAT))? else {
        return Ok(NativeStep::Return(Value::Void));
    };
    if bytes.len() > DATA_LIMIT || bytes.len() % 2 != 0 {
        return Err(NativeError::Message("invalid clipboard TJS data"));
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .take_while(|&u| u != 0)
        .collect();
    let text = Value::Str(cx.heap_mut().alloc_string(units));
    crate::scripts::evaluate_expression(cx, text)
}
fn arg(args: &[Value]) -> NativeResult<Value> {
    args.first().copied().ok_or(NativeError::Missing(0))
}
fn object(value: Value) -> NativeResult<ObjId> {
    if let Value::Obj(r) = value {
        r.object.ok_or(NativeError::This)
    } else {
        Err(NativeError::Type("an object"))
    }
}
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
#[derive(Clone, Copy)]
enum Stage {
    Text,
    Layer,
    Bitmap,
    Tjs,
    ValidateType,
    ValidateBody,
    Encoded,
    Image,
    Finish,
}
struct Write {
    input: Value,
    data: Data,
    stage: Stage,
    tjs: Value,
    missing: ObjId,
    checked: bool,
    api: Value,
}
impl Trace for Write {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.api.trace(visit);
        self.input.trace(visit);
        self.tjs.trace(visit);
        self.missing.trace(visit);
    }
}
impl Write {
    fn get(self: Box<Self>, cx: &mut NativeCx<'_>, object: Value, name: &str) -> NativeStep {
        NativeStep::GetRequiredOr {
            object,
            key: key(cx, name),
            fallback: Value::Obj(self.missing.into()),
            continuation: self,
        }
    }
    fn present(&self, v: Value) -> bool {
        !matches!(v, Value::Obj(r) if r.object == Some(self.missing))
    }
    fn next(self: Box<Self>) -> NativeStep {
        NativeStep::Continue(self)
    }
}
impl NativeContinuation for Write {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        // ncbPropAccessor first probes MUSTEXIST, then reads the member again.
        // Preserve both getter calls and the exact text/layer/tjs ordering.
        if !self.checked && self.present(value) {
            let name = match self.stage {
                Stage::Text => Some("text"),
                Stage::Layer => Some("layer"),
                Stage::Bitmap => Some("bitmap"),
                Stage::Tjs => Some("tjs"),
                _ => None,
            };
            if let Some(name) = name {
                self.checked = true;
                return Ok(NativeStep::GetRequired {
                    object: self.input,
                    key: key(cx, name),
                    continuation: self,
                });
            }
        }
        self.checked = false;
        match self.stage {
            Stage::Text => {
                if self.present(value) {
                    self.data.text = Some(text_units(cx, value)?);
                }
                self.stage = Stage::Layer;
                let input = self.input;
                Ok(self.get(cx, input, "layer"))
            }
            Stage::Layer | Stage::Bitmap => {
                if !self.present(value) && matches!(self.stage, Stage::Layer) {
                    self.stage = Stage::Bitmap;
                    let input = self.input;
                    return Ok(self.get(cx, input, "bitmap"));
                }
                self.stage = Stage::Image;
                if self.present(value) {
                    self.data.image_budget = Some(transfer_budget(cx, value)?);
                    crate::layer::read_pixels(cx, value, self)
                } else {
                    Ok(self.next())
                }
            }
            Stage::Image => {
                self.stage = Stage::Tjs;
                let input = self.input;
                Ok(self.get(cx, input, "tjs"))
            }
            Stage::Tjs => {
                if !self.present(value) {
                    self.stage = Stage::Finish;
                    return Ok(self.next());
                }
                object(value)?;
                self.tjs = value;
                self.stage = Stage::ValidateType;
                Ok(self.get(cx, value, "type"))
            }
            Stage::ValidateType | Stage::ValidateBody => {
                if !self.present(value) {
                    return Err(NativeError::Message(
                        "clipboard TJS data requires type and body",
                    ));
                }
                if matches!(self.stage, Stage::ValidateType) {
                    self.stage = Stage::ValidateBody;
                    let tjs = self.tjs;
                    return Ok(self.get(cx, tjs, "body"));
                }
                self.stage = Stage::Encoded;
                let data = self.tjs;
                serialize::start(cx, data, self.api, self)
            }
            Stage::Encoded => {
                let Value::Str(id) = value else {
                    return Err(NativeError::Type("serialized clipboard TJS"));
                };
                self.data.tjs = Some(
                    cx.heap()
                        .string(id)?
                        .iter()
                        .copied()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                );
                self.stage = Stage::Finish;
                Ok(self.next())
            }
            Stage::Finish => {
                if self.data.text.is_none() && self.data.tjs.is_none() && self.data.image.is_none()
                {
                    return Err(NativeError::Message(
                        "multiple clipboard data has no supported format",
                    ));
                }
                host(cx, |h| h.write(self.data))?;
                Ok(NativeStep::Return(Value::Void))
            }
        }
    }
}
impl crate::layer::PixelContinuation for Write {
    fn pixels(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        pixels: Arc<Pixels>,
    ) -> NativeResult<NativeStep> {
        self.data.image = Some(pixels);
        self.resume(cx, Value::Void)
    }
}
fn write(cx: &mut NativeCx<'_>, input: Value, stage: Stage) -> NativeResult<Box<Write>> {
    let this = cx.this();
    let api = cx
        .heap_mut()
        .with_native_state::<ArrayApi, _>(this, |s| s.0)?;
    Ok(Box::new(Write {
        api,
        input,
        stage,
        tjs: Value::Void,
        data: Data::default(),
        missing: cx.heap_mut().alloc_dictionary(),
        checked: false,
    }))
}
pub fn set_tjs(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args)?;
    let mut task = write(cx, input, Stage::Tjs)?;
    task.checked = true;
    task.resume(cx, input)
}
pub fn set_multiple(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args)?;
    object(input)?;
    Ok(write(cx, input, Stage::Text)?.get(cx, input, "text"))
}
pub fn set_bitmap(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args)?;
    let mut task = write(cx, input, Stage::Finish)?;
    task.data.image_budget = Some(transfer_budget(cx, input)?);
    crate::layer::read_pixels(cx, input, task)
}
pub fn get_bitmap(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let target = arg(args)?;
    if !host(cx, |h| h.has(2))? {
        return Ok(NativeStep::Return(Value::Int(0)));
    }
    let budget = crate::layer::pixel_budget(cx, target)?;
    let Some(pixels) = host(cx, |h| h.image(&budget))? else {
        return Ok(NativeStep::Return(Value::Int(0)));
    };
    crate::layer::write_pixels(cx, target, pixels)
}
struct ArrayApi(Value);
impl Trace for ArrayApi {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.0.trace(visit);
    }
}
/// Each link captures its own Array.count; bound exported closures keep it alive
/// after unlink, and releasing those closures releases the capture as well.
pub fn capture_array_count(heap: &mut Heap) -> NativeResult<ObjId> {
    let array = heap.registered_class("Array").ok_or(NativeError::This)?;
    let key = heap.intern(&[99, 111, 117, 110, 116]);
    let count = heap
        .member(array, key)?
        .ok_or(NativeError::Message("can't get Array.count"))?;
    if !matches!(count, Value::Obj(r) if r.object.is_some()) {
        return Err(NativeError::Type("Array.count property object"));
    }
    let context = heap.alloc_dictionary();
    heap.initialize_native_state(context, ArrayApi(count))?;
    Ok(context)
}
fn transfer_budget(cx: &mut NativeCx<'_>, source: Value) -> NativeResult<Budget> {
    match crate::window::clipboard::staging_budget(cx.heap_mut())? {
        Some(budget) => Ok(budget),
        None => crate::layer::pixel_budget(cx, source),
    }
}
pub fn get_watch(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    crate::window::clipboard::get(cx)
}
pub fn set_watch(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = arg(args)?;
    let enabled = value::to_integer(cx.heap(), value)? != 0;
    crate::window::clipboard::set(cx, enabled)?;
    Ok(Value::Void)
}
pub fn release_watches(heap: &mut Heap) -> NativeResult<()> {
    crate::window::clipboard::release(heap)
}
pub(crate) fn install(heap: &mut Heap) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)
}
pub fn set_host(heap: &mut Heap, host: impl Host + 'static) -> NativeResult<()> {
    let shared = service(heap)?;
    let mut service = shared.borrow_mut();
    if service.users != 0 {
        return Err(NativeError::Message(
            "cannot replace clipboard host while watching",
        ));
    }
    service.host = Some(Box::new(host));
    Ok(())
}
