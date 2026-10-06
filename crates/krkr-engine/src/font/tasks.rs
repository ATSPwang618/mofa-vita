use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
use krkr_assets::ReadPlan;
use krkr_protocol::text::{Metrics, Run, Style};
use std::sync::atomic::AtomicBool;
use tjs_core::{NativeContinuation, NativeStep, WaitMode};

pub(crate) enum Action {
    Characters(Vec<u16>),
    Measure(Vec<u16>, bool),
    Draw(Vec<u16>, Style, i32, i32),
    List(u32),
    Map(ReadPlan),
    Unmap,
    Register(ReadPlan),
}
pub(crate) struct Work {
    pub system: Arc<Mutex<krkr_render::font::System>>,
    pub font: Font,
    pub file: Option<ReadPlan>,
    pub action: Action,
    pub mapped: Option<Arc<krkr_render::font::prerendered::Font>>,
}
struct PendingWork {
    work: Work,
    operations: crate::operations::Shared,
    delivery: io::Delivery,
    next: Box<dyn NativeContinuation>,
}
impl Trace for PendingWork {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl PendingWork {
    fn queue(self) -> NativeResult<NativeStep> {
        Operations::wait(
            &self.operations,
            Request::Read(Box::new(io::Work::Font(self.work)), self.delivery),
            WaitMode::Internal,
            self.next,
        )
    }
}
pub(crate) fn execute(
    cx: &mut NativeCx<'_>,
    service: &Shared,
    work: Work,
    delivery: io::Delivery,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if matches!(&work.action, Action::Measure(_, false) | Action::Draw(..)) {
        // A worker can still be finishing cancelled work. Never block the VM
        // on its lock; queue normally if it is busy or needs a font load.
        let cached = work.system.try_lock().ok().and_then(|mut system| {
            // Explicit file fonts already retain their loaded face, too.
            // Require its first successful load before using cached metrics
            // so a missing file can never fall through to a mapped font.
            if work.font.file && !system.has_face(&work.font) {
                return None;
            }
            match &work.action {
                Action::Measure(text, false) => system
                    .measure_cached(&work.font, text, work.mapped.as_deref())
                    .map(Data::Metrics),
                Action::Draw(text, style, x, y) => system
                    .layout_cached(&work.font, text, *style, [*x, *y], work.mapped.as_ref())
                    .map(Data::Run),
                _ => None,
            }
        });
        if let Some(data) = cached {
            *delivery.borrow_mut() = Some(io::Data::Font(data));
            return next.resume(cx, Value::Void);
        }
    }
    let file = (work.font.file
        && matches!(
            &work.action,
            Action::Characters(..) | Action::Measure(..) | Action::Draw(..)
        ))
    .then(|| work.font.face.encode_utf16().collect::<Vec<_>>());
    let pending = PendingWork {
        work,
        operations: service.operations.clone(),
        delivery,
        next,
    };
    if let Some(file) = file {
        crate::storages::managed::plans(
            cx,
            vec![(file, true)],
            pending,
            |mut pending, _, mut plans| {
                pending.work.file = plans.pop().flatten();
                pending.queue()
            },
        )
    } else {
        pending.queue()
    }
}
pub(crate) enum Data {
    Characters(i32, Vec<(u16, i32)>),
    Metrics(Metrics),
    Run(Run),
    List(Vec<String>),
    Mapped(Mapping),
    Unmapped,
    Registered(krkr_render::font::Registration),
}
impl Work {
    pub fn run(self, stop: &AtomicBool) -> NativeResult<Data> {
        let run = || -> krkr_render::Result<Data> {
            let mut system = self.system.lock().unwrap();
            if matches!(&self.action, Action::Draw(..) | Action::Measure(..)) {
                system.set_mapping(self.font.clone(), self.mapped);
            }
            if let Some(plan) = self.file
                && !system.has_face(&self.font)
            {
                let permit = system.reserve(plan.bytes as usize)?;
                let bytes = plan
                    .read_interruptible(0, || stop.load(std::sync::atomic::Ordering::Relaxed))
                    .map_err(|e| krkr_render::Error::Backend(e.to_string()))?;
                system.insert_face(
                    &self.font,
                    krkr_render::font::Face::from_bytes(bytes, 0, permit)?,
                );
            }
            match self.action {
                Action::Characters(chars) => {
                    system.set_mapping(self.font.clone(), None);
                    let ascent = system.ascent(&self.font)?;
                    let mut widths = Vec::with_capacity(chars.len());
                    for c in chars {
                        widths.push((c, system.measure(&self.font, &[c], false, stop)?.width));
                    }
                    Ok(Data::Characters(ascent, widths))
                }
                Action::Measure(text, bounds) => system
                    .measure(&self.font, &text, bounds, stop)
                    .map(Data::Metrics),
                Action::Draw(text, style, x, y) => system
                    .layout(&self.font, &text, style, x, y, stop)
                    .map(Data::Run),
                Action::List(flags) => system.list(&self.font, flags).map(Data::List),
                Action::Map(plan) => {
                    let stream = plan
                        .open_interruptible(&|| stop.load(std::sync::atomic::Ordering::Relaxed))
                        .map_err(|e| krkr_render::Error::Backend(e.to_string()))?;
                    let font = Arc::new(krkr_render::font::prerendered::Font::open(
                        stream,
                        plan.bytes,
                        |bytes| system.reserve(bytes),
                        stop,
                    )?);
                    Ok(Data::Mapped(Mapping { source: plan, font }))
                }
                Action::Unmap => {
                    // Only discard worker caches here. The visible mapping is
                    // removed on resume; cancellation restores it on next use.
                    system.unmap(&self.font);
                    Ok(Data::Unmapped)
                }
                Action::Register(plan) => {
                    let permit = system.reserve(plan.bytes as usize)?;
                    let bytes = plan
                        .read_interruptible(0, || stop.load(std::sync::atomic::Ordering::Relaxed))
                        .map_err(|e| krkr_render::Error::Backend(e.to_string()))?;
                    system
                        .prepare_registration(bytes, permit, stop)
                        .map(Data::Registered)
                }
            }
        };
        run().map_err(|e| NativeError::Detail(e.to_string()))
    }
}
#[derive(Clone, Copy)]
pub(super) enum Query {
    Width,
    Height,
    WidthX,
    WidthY,
    HeightX,
    HeightY,
    Bounds,
    List,
    RegisteredNames,
    Done,
}
struct Returned {
    service: Shared,
    font: Font,
    owner: ObjId,
    delivery: io::Delivery,
    query: Query,
    angle: i32,
}
impl Trace for Returned {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Font(data)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected font response"));
        };
        let value = match data {
            Data::Metrics(m) => {
                let (s, c) = (self.angle as f64 * std::f64::consts::PI / 1800.0).sin_cos();
                match self.query {
                    Query::Width => Value::Int(m.width.into()),
                    Query::Height => Value::Int(m.height.into()),
                    Query::WidthX => Value::Real(c * m.width as f64),
                    Query::WidthY => Value::Real(-s * m.width as f64),
                    Query::HeightX => Value::Real(s * m.height as f64),
                    Query::HeightY => Value::Real(c * m.height as f64),
                    Query::Bounds => crate::rect::object(cx.heap_mut(), m.bounds)?,
                    _ => return Err(NativeError::Message("unexpected font metrics")),
                }
            }
            Data::List(names) => names_value(cx, names)?,
            Data::Mapped(font) => {
                self.service.mapped.borrow_mut().insert(self.font, font);
                Value::Void
            }
            Data::Unmapped => {
                self.service.mapped.borrow_mut().remove(&self.font);
                Value::Void
            }
            Data::Registered(fonts) => {
                let names = matches!(self.query, Query::RegisteredNames).then(|| fonts.names());
                let count = self.service.worker.lock().unwrap().register(fonts);
                match names {
                    Some(names) => names_value(cx, names)?,
                    None => Value::Int(count as i64),
                }
            }
            _ => return Err(NativeError::Message("unexpected font response")),
        };
        Ok(NativeStep::Return(value))
    }
}
fn names_value(cx: &mut NativeCx<'_>, names: Vec<String>) -> NativeResult<Value> {
    let values = names
        .into_iter()
        .map(|name| {
            Value::Str(
                cx.heap_mut()
                    .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
            )
        })
        .collect();
    let array = cx.heap_mut().alloc_array();
    cx.heap_mut().array_replace(array, values)?;
    Ok(Value::Obj(tjs_core::ObjRef::bound(array)))
}
pub(crate) fn register(cx: &mut NativeCx<'_>, filename: &[u16]) -> NativeResult<NativeStep> {
    register_query(cx, filename, Query::Done)
}
pub(crate) fn register_names(cx: &mut NativeCx<'_>, filename: &[u16]) -> NativeResult<NativeStep> {
    register_query(cx, filename, Query::RegisteredNames)
}
fn register_query(
    cx: &mut NativeCx<'_>,
    filename: &[u16],
    query: Query,
) -> NativeResult<NativeStep> {
    // Keep the original System.addFont plugin's count/void contract separate
    // from Font.addFont's family-name array, sharing the same registration IO.
    crate::storages::managed::plans(
        cx,
        vec![(filename.to_vec(), matches!(query, Query::RegisteredNames))],
        matches!(query, Query::RegisteredNames),
        |names, cx, mut plans| {
            let Some(plan) = plans.pop().flatten() else {
                return Ok(NativeStep::Return(Value::Void));
            };
            let service = super::service(cx)?;
            let font = Font::default();
            let work = work(cx, &service, font.clone(), Action::Register(plan))?;
            let delivery = io::Delivery::default();
            execute(
                cx,
                &service,
                work,
                delivery.clone(),
                Box::new(Returned {
                    service: service.clone(),
                    font,
                    owner: cx.this(),
                    delivery,
                    query: if names {
                        Query::RegisteredNames
                    } else {
                        Query::Done
                    },
                    angle: 0,
                }),
            )
        },
    )
}
pub(crate) fn work(
    _cx: &mut NativeCx<'_>,
    service: &Shared,
    font: Font,
    action: Action,
) -> NativeResult<Work> {
    Ok(Work {
        mapped: service.mapped.borrow().get(&font).map(|m| m.font.clone()),
        system: service.worker.clone(),
        font,
        file: None,
        action,
    })
}
impl State {
    pub(super) fn query(
        &self,
        cx: &mut NativeCx<'_>,
        action: Action,
        query: Query,
    ) -> NativeResult<NativeStep> {
        self.target.require_main()?;
        let font = self.target.read(Clone::clone)?;
        let angle = font.angle;
        if matches!(self.target, Target::Owned(_)) {
            let height = f64::from(font.height.saturating_abs());
            let (s, c) = (angle as f64 * std::f64::consts::PI / 1800.0).sin_cos();
            let immediate = match query {
                Query::Height => Some(Value::Int(font.height.saturating_abs().into())),
                Query::HeightX => Some(Value::Real(s * height)),
                Query::HeightY => Some(Value::Real(c * height)),
                _ => None,
            };
            if let Some(value) = immediate {
                return Ok(NativeStep::Return(value));
            }
        }
        let service = self.service.as_ref().ok_or(NativeError::This)?;
        // Repeated character-layer setup maps the same font again. Preserve
        // its identity so the worker and GPU can keep cached glyph masks.
        // `plan` was freshly resolved; changed files and dynamic media still
        // take the normal cancellable load path.
        if let Action::Map(plan) = &action
            && service
                .mapped
                .borrow()
                .get(&font)
                .is_some_and(|mapped| mapped.source.same_file_version(plan))
        {
            return Ok(NativeStep::Return(Value::Void));
        }
        let work = work(cx, service, font.clone(), action)?;
        let delivery = io::Delivery::default();
        execute(
            cx,
            service,
            work,
            delivery.clone(),
            Box::new(Returned {
                service: service.clone(),
                font,
                owner: cx.this(),
                delivery,
                query,
                angle,
            }),
        )
    }
}
