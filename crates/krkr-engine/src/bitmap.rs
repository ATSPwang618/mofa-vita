//! Explicit CPU bitmap objects. Pixels are budgeted and shared until mutation;
//! drawing uploads a temporary source into the existing GPU command pipeline.
use crate::{
    io,
    operations::{self, Operations, Request},
};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef,
    RestArgs, Trace, Value, WaitMode, value,
};

#[derive(Clone)]
struct Service {
    operations: operations::Shared,
    budget: Budget,
}
#[derive(Default)]
struct Data {
    pixels: Option<Arc<Pixels>>,
}
type Shared = Rc<RefCell<Data>>;
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
fn size(width: i64, height: i64) -> NativeResult<Size> {
    if width <= 0 || height <= 0 || width > 16384 || height > 16384 {
        return Err(NativeError::Message(
            "bitmap dimensions must be in 1..16384",
        ));
    }
    Ok(Size {
        width: width as u32,
        height: height as u32,
    })
}
fn blank(size: Size, budget: &Budget) -> NativeResult<Pixels> {
    Ok(Pixels {
        size,
        main: Some(
            Bytes::zeroed(
                size.rgba_bytes()
                    .ok_or(NativeError::Message("bitmap size overflow"))?,
                budget,
            )
            .map_err(error)?,
        ),
        province: None,
    })
}
fn copy(pixels: &Pixels, budget: &Budget) -> NativeResult<Pixels> {
    let mut out = blank(pixels.size, budget)?;
    out.main.as_mut().unwrap().as_mut_slice().copy_from_slice(
        pixels
            .main
            .as_ref()
            .ok_or(NativeError::Message("bitmap has no main plane"))?
            .as_slice(),
    );
    Ok(out)
}
fn object(v: Value) -> NativeResult<ObjId> {
    if let Value::Obj(r) = v {
        r.object.ok_or(NativeError::This)
    } else {
        Err(NativeError::This)
    }
}
fn service(cx: &mut NativeCx<'_>) -> NativeResult<Service> {
    let class = cx
        .heap()
        .registered_class("Bitmap")
        .ok_or(NativeError::This)?;
    cx.heap_mut()
        .with_native_state::<implementation::State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::This)
}
pub(crate) fn snapshot(heap: &mut Heap, value: Value) -> NativeResult<Arc<Pixels>> {
    heap.with_native_state::<implementation::State, _>(object(value)?, |s| s.pixels())?
}

/// A rooted destination for an ordered GPU readback. The byte allocation moves
/// from the host staging pool to the Bitmap pool without another CPU copy.
pub(crate) struct Replacement {
    owner: ObjId,
    data: Shared,
    budget: Budget,
}
impl Trace for Replacement {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.owner.into()));
    }
}
impl Replacement {
    pub(crate) fn commit(self, mut pixels: Pixels) -> NativeResult<()> {
        let main = pixels
            .main
            .take()
            .ok_or(NativeError::Message("readback has no main plane"))?;
        let (data, staging_permit) = main.into_parts();
        if pixels.size.rgba_bytes() != Some(data.len()) {
            return Err(NativeError::Message("readback bitmap size mismatch"));
        }
        let permit = self.budget.reserve(data.capacity()).map_err(error)?;
        pixels.main = Some(Bytes::with_permit(data, permit));
        pixels.province = None;
        drop(staging_permit);
        let mut destination = self.data.borrow_mut();
        if destination.pixels.is_none() {
            return Err(NativeError::Message("bitmap is not drawable"));
        }
        destination.pixels = Some(Arc::new(pixels));
        Ok(())
    }
}
pub(crate) fn replacement(heap: &mut Heap, value: Value) -> NativeResult<Replacement> {
    let owner = object(value)?;
    heap.with_native_state::<implementation::State, _>(owner, |state| {
        state.pixels()?;
        Ok(Replacement {
            owner,
            data: state.data.clone(),
            budget: state
                .service
                .as_ref()
                .ok_or(NativeError::This)?
                .budget
                .clone(),
        })
    })?
}

#[tjs_bind::class(name = "Bitmap")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) service: Option<Service>,
        pub(super) data: Shared,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        pub(super) fn pixels(&self) -> NativeResult<Arc<Pixels>> {
            self.data
                .borrow()
                .pixels
                .clone()
                .ok_or(NativeError::Message("bitmap is not drawable"))
        }
        fn resize(&self, size: Size) -> NativeResult<()> {
            let pixels = self.pixels()?;
            if size == pixels.size {
                return Ok(());
            }
            let mut out = blank(
                size,
                &self.service.as_ref().ok_or(NativeError::This)?.budget,
            )?;
            let width = size.width.min(pixels.size.width) as usize * 4;
            let src = pixels.main.as_ref().unwrap().as_slice();
            let dest = out.main.as_mut().unwrap().as_mut_slice();
            for row in 0..size.height.min(pixels.size.height) as usize {
                dest[row * size.width as usize * 4..][..width]
                    .copy_from_slice(&src[row * pixels.size.width as usize * 4..][..width]);
            }
            self.data.borrow_mut().pixels = Some(Arc::new(out));
            Ok(())
        }
        fn pixel(&self, x: i64, y: i64, update: Option<(u32, bool)>) -> NativeResult<i64> {
            let mut data = self.data.borrow_mut();
            let pixels = data
                .pixels
                .as_mut()
                .ok_or(NativeError::Message("bitmap is not drawable"))?;
            if x < 0 || y < 0 || x >= pixels.size.width as i64 || y >= pixels.size.height as i64 {
                return Err(NativeError::Message("bitmap pixel is outside image"));
            }
            let at = (y as usize * pixels.size.width as usize + x as usize) * 4;
            if let Some((color, mask)) = update {
                if Arc::get_mut(pixels).is_none() {
                    *pixels = Arc::new(copy(
                        pixels,
                        &self.service.as_ref().ok_or(NativeError::This)?.budget,
                    )?);
                }
                let p = &mut Arc::get_mut(pixels)
                    .unwrap()
                    .main
                    .as_mut()
                    .unwrap()
                    .as_mut_slice()[at..at + 4];
                if mask {
                    p[3] = color as u8;
                } else {
                    p[..3].copy_from_slice(&[(color >> 16) as u8, (color >> 8) as u8, color as u8]);
                }
                Ok(0)
            } else {
                let p = &pixels.main.as_ref().unwrap().as_slice()[at..at + 4];
                Ok(u32::from_be_bytes([p[3], p[0], p[1], p[2]]) as i64)
            }
        }
        #[tjs::constructor(resumable = true)]
        fn create(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let service = service(cx)?;
            let state = Self {
                service: Some(service.clone()),
                data: Shared::default(),
            };
            if let Some(Value::Str(_)) = args.first() {
                let name = value::to_string_units(cx.heap(), args[0])?;
                let key = args
                    .get(1)
                    .map(|v| value::to_integer(cx.heap(), *v))
                    .transpose()?
                    .unwrap_or(0x02ffffff) as u32;
                return load(cx, service, state.data.clone(), &name, key, Some(state));
            }
            let dims = if args.is_empty() {
                size(32, 32)?
            } else {
                let width = value::to_integer(cx.heap(), args[0])?;
                let height =
                    value::to_integer(cx.heap(), *args.get(1).ok_or(NativeError::Missing(1))?)?;
                if let Some(bpp) = args.get(2)
                    && value::to_integer(cx.heap(), *bpp)? != 32
                {
                    return Err(NativeError::Message(
                        "Bitmap currently requires 32-bit pixels",
                    ));
                }
                size(width, height)?
            };
            let mut pixels = blank(dims, &service.budget)?;
            if args.is_empty() {
                for pixel in pixels
                    .main
                    .as_mut()
                    .unwrap()
                    .as_mut_slice()
                    .as_chunks_mut::<4>()
                    .0
                {
                    pixel[..3].fill(255);
                }
            }
            state.data.borrow_mut().pixels = Some(Arc::new(pixels));
            Ok(NativeStep::Return(cx.construct(state)?))
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.data.borrow_mut().pixels = None;
        }
        #[tjs::getter]
        fn width(&self) -> NativeResult<i64> {
            Ok(self.pixels()?.size.width.into())
        }
        #[tjs::getter]
        fn height(&self) -> NativeResult<i64> {
            Ok(self.pixels()?.size.height.into())
        }
        #[tjs::setter(name = "width")]
        fn set_width(&self, value: i64) -> NativeResult<()> {
            self.resize(size(value, self.pixels()?.size.height.into())?)
        }
        #[tjs::setter(name = "height")]
        fn set_height(&self, value: i64) -> NativeResult<()> {
            self.resize(size(self.pixels()?.size.width.into(), value)?)
        }
        #[tjs::method(name = "setSize")]
        fn set_size(&self, width: i64, height: i64) -> NativeResult<()> {
            self.resize(size(width, height)?)
        }
        #[tjs::getter]
        fn loading(&self) -> bool {
            false
        }
        #[tjs::method(name = "getPixel")]
        fn get_pixel(&self, x: i64, y: i64) -> NativeResult<i64> {
            Ok(self.pixel(x, y, None)? & 0xffffff)
        }
        #[tjs::method(name = "getMaskPixel")]
        fn get_mask_pixel(&self, x: i64, y: i64) -> NativeResult<i64> {
            Ok(self.pixel(x, y, None)? >> 24)
        }
        #[tjs::method(name = "setPixel")]
        fn set_pixel(&self, x: i64, y: i64, color: i64) -> NativeResult<()> {
            self.pixel(x, y, Some((crate::color::actual(color as u32), false)))
                .map(|_| ())
        }
        #[tjs::method(name = "setMaskPixel")]
        fn set_mask_pixel(&self, x: i64, y: i64, color: i64) -> NativeResult<()> {
            self.pixel(x, y, Some((color as u32, true))).map(|_| ())
        }
        #[tjs::method(name = "copyFrom")]
        fn copy_from(&self, cx: &mut NativeCx<'_>, source: Value) -> NativeResult<()> {
            if matches!(source, Value::Obj(ObjRef { object: None, .. })) {
                return Ok(());
            }
            if object(source)? != cx.this() {
                self.data.borrow_mut().pixels = Some(snapshot(cx.heap_mut(), source)?);
            }
            Ok(())
        }
        #[tjs::method]
        fn independ(&self, cx: &NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            let copy_image = args
                .first()
                .filter(|v| !matches!(v, Value::Void))
                .map(|v| v.truthy(cx.heap()))
                .transpose()?
                .unwrap_or(true);
            let mut data = self.data.borrow_mut();
            let pixels = data
                .pixels
                .as_mut()
                .ok_or(NativeError::Message("bitmap is not drawable"))?;
            if Arc::get_mut(pixels).is_none() {
                let budget = &self.service.as_ref().ok_or(NativeError::This)?.budget;
                *pixels = Arc::new(if copy_image {
                    copy(pixels, budget)?
                } else {
                    blank(pixels.size, budget)?
                });
            }
            Ok(())
        }
        #[tjs::method(resumable = true)]
        fn load(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let name =
                value::to_string_units(cx.heap(), *args.first().ok_or(NativeError::Missing(0))?)?;
            let key = args
                .get(1)
                .map(|v| value::to_integer(cx.heap(), *v))
                .transpose()?
                .unwrap_or(0x02ffffff) as u32;
            super::load(
                cx,
                self.service.clone().ok_or(NativeError::This)?,
                self.data.clone(),
                &name,
                key,
                None,
            )
        }
        #[tjs::method(resumable = true)]
        fn save(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let service = self.service.as_ref().ok_or(NativeError::This)?;
            let name =
                value::to_string_units(cx.heap(), *args.first().ok_or(NativeError::Missing(0))?)?;
            let mode = args
                .get(1)
                .map(|v| value::to_string_units(cx.heap(), *v))
                .transpose()?
                .unwrap_or_else(|| "bmp".encode_utf16().collect());
            let format =
                krkr_image::save::Format::parse(&String::from_utf16_lossy(&mode)).map_err(error)?;
            let target = crate::storages::service(cx)?
                .borrow()
                .write_plan(&name)
                .map_err(error)?;
            let pixels = copy(self.pixels()?.as_ref(), &service.budget)?;
            let work = io::Work::ImageSave(krkr_image::save::Request {
                target,
                format,
                pixels,
                tags: vec![],
                budget: service.budget.clone(),
            });
            let delivery = io::Delivery::default();
            Operations::wait(
                &service.operations,
                Request::Read(Box::new(work), delivery.clone()),
                WaitMode::Internal,
                Box::new(Saved(delivery)),
            )
        }
    }
}
struct Saved(io::Delivery);
impl Trace for Saved {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Saved {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::FileWritten(path)) = self.0.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected bitmap save response"));
        };
        crate::storages::service(cx)?
            .borrow_mut()
            .invalidate_file(&path);
        Ok(NativeStep::Return(Value::Void))
    }
}
struct Loading {
    service: Service,
    data: Shared,
    owner: ObjId,
    constructor: Option<implementation::State>,
    delivery: io::Delivery,
}
impl Trace for Loading {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.owner.into()));
    }
}
impl NativeContinuation for Loading {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let response = self
            .delivery
            .borrow_mut()
            .take()
            .ok_or(NativeError::Message("missing bitmap response"))?;
        match response {
            io::Data::ImagePrepared(prepared) => {
                let delivery = io::Delivery::default();
                self.delivery = delivery.clone();
                let operations = self.service.operations.clone();
                Operations::wait(
                    &operations,
                    Request::Read(Box::new(io::Work::ImageDecode(*prepared)), delivery),
                    WaitMode::Internal,
                    self,
                )
            }
            io::Data::Image(mut decoded) => {
                // Bitmap has no separate province plane (unlike Layer).
                decoded.pixels.province = None;
                self.data.borrow_mut().pixels = Some(Arc::new(decoded.pixels));
                if let Some(state) = self.constructor.take() {
                    return Ok(NativeStep::Return(cx.construct(state)?));
                }
                let value = if decoded.tags.is_empty() {
                    Value::Obj(ObjRef {
                        object: None,
                        this: None,
                    })
                } else {
                    let dict = cx.heap_mut().alloc_dictionary();
                    for (key, value) in decoded.tags {
                        let key = cx.heap_mut().intern_str(&key);
                        let value = Value::Str(
                            cx.heap_mut()
                                .alloc_string(value.encode_utf16().collect::<Vec<_>>()),
                        );
                        cx.heap_mut().set_member(dict, key, value)?;
                    }
                    Value::Obj(dict.into())
                };
                Ok(NativeStep::Return(value))
            }
            _ => Err(NativeError::Message("unexpected bitmap response")),
        }
    }
}
fn load(
    cx: &mut NativeCx<'_>,
    service: Service,
    data: Shared,
    name: &[u16],
    key: u32,
    constructor: Option<implementation::State>,
) -> NativeResult<NativeStep> {
    let delivery = io::Delivery::default();
    let options = crate::storages::image::Options {
        name: name.to_vec(),
        key,
        size: None,
        grayscale: false,
        budget: service.budget.clone(),
    };
    crate::storages::image::request(
        cx,
        options,
        Loading {
            service,
            data,
            owner: cx.this(),
            constructor,
            delivery,
        },
        |load, _, mut request| {
            request.province = None;
            Operations::wait(
                &load.service.operations.clone(),
                Request::Read(
                    Box::new(io::Work::ImageProbe(request)),
                    load.delivery.clone(),
                ),
                WaitMode::Internal,
                Box::new(load),
            )
        },
    )
}
pub(crate) fn install(heap: &mut Heap, operations: operations::Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |state| {
        state.service = Some(Service {
            operations,
            budget: Budget::new(32 * 1024 * 1024),
        })
    })
}

pub(crate) fn set_budget(heap: &mut Heap, bytes: usize) -> NativeResult<()> {
    let class = heap.registered_class("Bitmap").expect("installed Bitmap");
    heap.with_native_state::<implementation::State, _>(class, |state| {
        let service = state.service.as_mut().expect("Bitmap service");
        if service.budget.used() != 0 {
            return Err(NativeError::Message(
                "configure Bitmap budget before allocating images",
            ));
        }
        service.budget = Budget::new(bytes);
        service.budget.set_profile_name("memory.bitmap_bytes");
        Ok(())
    })?
}

/// Clipboard and other portable pixel services use the Bitmap's own CPU pool.
pub(crate) fn pixel_budget(heap: &mut Heap, value: Value) -> NativeResult<Option<Budget>> {
    match heap.with_native_state::<implementation::State, _>(object(value)?, |s| {
        s.pixels()?;
        Ok(s.service.as_ref().ok_or(NativeError::This)?.budget.clone())
    }) {
        Ok(result) => result.map(Some),
        Err(NativeError::This) => Ok(None),
        Err(error) => Err(error),
    }
}
pub(crate) fn replace_pixels(heap: &mut Heap, value: Value, pixels: Pixels) -> NativeResult<()> {
    heap.with_native_state::<implementation::State, _>(object(value)?, |s| {
        s.pixels()?;
        s.data.borrow_mut().pixels = Some(Arc::new(pixels));
        Ok(())
    })?
}
