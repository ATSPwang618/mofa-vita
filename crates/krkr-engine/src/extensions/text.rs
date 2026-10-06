//! Owned metadata queries for text-layout plugins. IO stays on the shared worker.
use super::*;
use crate::{
    font, io,
    operations::{Operations, Request},
};
use tjs_core::{NativeContinuation, NativeError, Trace, WaitMode};
pub trait FontMetricsContinuation: Trace {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        ascent: i32,
        widths: Vec<(u16, i32)>,
    ) -> NativeResult<NativeStep>;
}
pub trait ImageSizeContinuation: Trace {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, size: Size) -> NativeResult<NativeStep>;
}
struct FontReply {
    delivery: io::Delivery,
    next: Box<dyn FontMetricsContinuation>,
}
impl Trace for FontReply {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl NativeContinuation for FontReply {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Font(font::tasks::Data::Characters(ascent, widths))) =
            self.delivery.borrow_mut().take()
        else {
            return Err(NativeError::Message(
                "unexpected character metrics response",
            ));
        };
        self.next.resume(cx, ascent, widths)
    }
}
pub fn measure_characters(
    cx: &mut NativeCx<'_>,
    font: crate::protocol::text::Font,
    mut chars: Vec<u16>,
    next: Box<dyn FontMetricsContinuation>,
) -> NativeResult<NativeStep> {
    chars.sort_unstable();
    chars.dedup();
    let service = font::service(cx)?;
    let work = font::tasks::work(cx, &service, font, font::tasks::Action::Characters(chars))?;
    let delivery = io::Delivery::default();
    font::tasks::execute(
        cx,
        &service,
        work,
        delivery.clone(),
        Box::new(FontReply { delivery, next }),
    )
}
struct ImageReply {
    delivery: io::Delivery,
    operations: crate::operations::Shared,
    next: Box<dyn ImageSizeContinuation>,
}
impl Trace for ImageReply {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl NativeContinuation for ImageReply {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::ImagePrepared(prepared)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected image size response"));
        };
        self.next.resume(cx, prepared.size)
    }
}
pub fn image_size(
    cx: &mut NativeCx<'_>,
    name: &[u16],
    next: Box<dyn ImageSizeContinuation>,
) -> NativeResult<NativeStep> {
    let service = font::service(cx)?;
    let budget = image_staging_budget(cx.heap_mut())?
        .ok_or(NativeError::Message("image staging budget is unavailable"))?;
    let delivery = io::Delivery::default();
    let options = crate::storages::image::Options {
        name: name.to_vec(),
        key: 0x02ffffff,
        size: None,
        grayscale: false,
        budget,
    };
    crate::storages::image::request(
        cx,
        options,
        ImageReply {
            delivery,
            next,
            operations: service.operations.clone(),
        },
        |reply, _, request| {
            Operations::wait(
                &reply.operations.clone(),
                Request::Read(
                    Box::new(io::Work::ImageProbe(request)),
                    reply.delivery.clone(),
                ),
                WaitMode::Internal,
                Box::new(reply),
            )
        },
    )
}
