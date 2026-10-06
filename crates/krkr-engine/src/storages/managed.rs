//! VM-owned storage resolution. A provider may yield through property getters;
//! only the resolved file bytes enter a Send read plan.
use super::*;
use krkr_assets::{ReadSource, Search, StorageMedium, Stream};
use std::{collections::BTreeMap, io::Cursor, sync::Arc};
use tjs_core::{NativeContinuation, NativeStep};

#[derive(Clone, Copy)]
pub enum Operation {
    Read,
    Exists,
    List,
    Write,
    Update,
}
/// Read returns an Octet or void when absent. Exists returns a boolean, List
/// an Array of names. Write validates the destination; Update additionally
/// reads its current member before discarding the temporary write buffer.
pub trait Medium: Trace {
    fn resolve(
        &self,
        cx: &mut NativeCx<'_>,
        path: Vec<u16>,
        operation: Operation,
        next: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep>;
}
#[derive(Default)]
struct Registry {
    serial: u64,
    entries: BTreeMap<Vec<u16>, (u64, Rc<dyn Medium>)>,
}
impl Trace for Registry {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for (_, medium) in self.entries.values() {
            medium.trace(visit);
        }
    }
}
#[derive(Clone, Default)]
pub(super) struct Managed(Rc<RefCell<Registry>>);
impl Trace for Managed {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.0.borrow().trace(visit);
    }
}
fn registry(heap: &mut Heap) -> NativeResult<Managed> {
    let class = heap.registered_class("Storages").ok_or(NativeError::This)?;
    heap.with_native_state::<super::implementation::State, _>(class, |state| state.managed.clone())
}
struct Marker;
impl StorageMedium for Marker {
    fn normalize(&self, path: &[u16]) -> Vec<u16> {
        path.to_vec()
    }
    fn plan(&self, _: &mut Vfs, _: &[u16]) -> krkr_assets::Result<Option<ReadPlan>> {
        Err(krkr_assets::Error::Format(
            "managed storage requires VM resolution",
        ))
    }
    fn list(&self, _: &mut Vfs, _: &[u16]) -> krkr_assets::Result<Vec<Vec<u16>>> {
        Err(krkr_assets::Error::Format(
            "managed directory requires VM resolution",
        ))
    }
}
/// Dropping registration removes future lookup; active continuations retain
/// their provider independently and completed read plans retain only bytes.
pub struct Registration {
    registry: Managed,
    name: String,
    key: Vec<u16>,
    serial: u64,
    vfs: Shared,
    marker: Arc<dyn StorageMedium>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut registry = self.registry.0.borrow_mut();
        if registry
            .entries
            .get(&self.key)
            .is_some_and(|(id, _)| *id == self.serial)
        {
            registry.entries.remove(&self.key);
        }
        self.vfs
            .borrow_mut()
            .unregister_medium(&self.name, &self.marker);
    }
}
pub fn register(
    heap: &mut Heap,
    name: &str,
    provider: Rc<dyn Medium>,
) -> NativeResult<Registration> {
    let registry = registry(heap)?;
    let vfs = service_from_heap(heap)?;
    let key = krkr_assets::name::units(&name.to_ascii_lowercase());
    let marker: Arc<dyn StorageMedium> = Arc::new(Marker);
    let serial = {
        let mut registry = registry.0.borrow_mut();
        if registry.entries.contains_key(&key) {
            return Err(NativeError::Message(
                "managed storage medium is already registered",
            ));
        }
        let serial = registry.serial.checked_add(1).ok_or(NativeError::Message(
            "managed registration identity exhausted",
        ))?;
        vfs.borrow_mut()
            .register_medium(name, marker.clone())
            .map_err(error)?;
        registry.serial = serial;
        registry.entries.insert(key.clone(), (serial, provider));
        serial
    };
    Ok(Registration {
        registry,
        name: name.into(),
        key,
        serial,
        vfs,
        marker,
    })
}
pub struct Resolution {
    provider: Rc<dyn Medium>,
    pub path: Vec<u16>,
}
impl Trace for Resolution {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.provider.trace(visit);
    }
}
impl Resolution {
    pub fn start(
        self,
        cx: &mut NativeCx<'_>,
        operation: Operation,
        next: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep> {
        let next = Box::new(Active {
            provider: self.provider.clone(),
            next,
        });
        self.provider.resolve(cx, self.path, operation, next)
    }
}
struct Active {
    provider: Rc<dyn Medium>,
    next: Box<dyn NativeContinuation>,
}
impl Trace for Active {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.provider.trace(visit);
        self.next.trace(visit);
    }
}
impl NativeContinuation for Active {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        self.next.resume(cx, value)
    }
}
/// Lookup does not invoke script. The caller releases all service borrows
/// before starting the returned resolution through the normal VM scheduler.
pub fn find(cx: &mut NativeCx<'_>, path: &[u16]) -> NativeResult<Option<Resolution>> {
    let managed = registry(cx.heap_mut())?;
    let vfs = service(cx)?;
    let path = vfs.borrow().full_path(path).map_err(error)?;
    Ok(find_normalized(&managed, &path))
}
fn find_normalized(managed: &Managed, path: &[u16]) -> Option<Resolution> {
    let at = path.iter().position(|&v| v == 58)?;
    let provider = managed
        .0
        .borrow()
        .entries
        .get(&path[..at])
        .map(|(_, medium)| medium.clone());
    provider.map(|provider| Resolution {
        provider,
        path: path.to_vec(),
    })
}
fn find_candidate(
    cx: &mut NativeCx<'_>,
    managed: &Managed,
    path: &[u16],
) -> NativeResult<Option<Resolution>> {
    let Some(mut resolution) = find_normalized(managed, path) else {
        return Ok(None);
    };
    // Auto-path prefixes are normalized already, but script media can own
    // additional normalization rules for the appended basename.
    resolution.path = service(cx)?.borrow().full_path(path).map_err(error)?;
    Ok(Some(resolution))
}
struct Bytes(Arc<[u8]>);
pub fn placed(
    cx: &mut NativeCx<'_>,
    path: &[u16],
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let search = service(cx)?.borrow_mut().search(path).map_err(error)?;
    let candidates = match search {
        Search::Found { candidate, .. } => {
            let value = string(cx, candidate);
            return next.resume(cx, value);
        }
        Search::Candidates(candidates) => candidates.into_iter(),
    };
    Placed {
        candidates,
        pending: Vec::new(),
        next,
    }
    .advance(cx)
}
struct Placed {
    candidates: std::vec::IntoIter<Vec<u16>>,
    pending: Vec<u16>,
    next: Box<dyn NativeContinuation>,
}
impl Trace for Placed {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl Placed {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        // Sibling auto paths often search different directories in one XP3.
        // Verify its index once here; discard before any script provider runs.
        let mut lookup = krkr_assets::Lookup::default();
        let vfs = service(cx)?;
        let managed = registry(cx.heap_mut())?;
        while let Some(path) = self.candidates.next() {
            let candidate_timer = krkr_protocol::diagnostics::Timer::start();
            if let Some(resolution) = find_candidate(cx, &managed, &path)? {
                self.pending = resolution.path.clone();
                drop(lookup);
                return resolution.start(cx, Operation::Exists, Box::new(self));
            }
            let plan = vfs
                .borrow_mut()
                .direct_plan_in(&path, &mut lookup)
                .map_err(error)?;
            candidate_timer.report(|| {
                format!(
                    "stage=storage-candidate name={}",
                    String::from_utf16_lossy(&path)
                )
            });
            if plan.is_some() {
                drop(lookup);
                let value = string(cx, path);
                return self.next.resume(cx, value);
            }
        }
        drop(lookup);
        let empty = string(cx, Vec::new());
        self.next.resume(cx, empty)
    }
}
impl NativeContinuation for Placed {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        if value.truthy(cx.heap())? {
            let value = string(cx, self.pending);
            self.next.resume(cx, value)
        } else {
            self.advance(cx)
        }
    }
}
pub(super) fn container_io(
    cx: &mut NativeCx<'_>,
    request: tjs_core::storage::Request,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if matches!(
        request.io,
        tjs_core::storage::Io::ReadText | tjs_core::storage::Io::ReadBinary
    ) {
        return plans(
            cx,
            vec![(request.name.clone(), true)],
            ContainerIo { request, next },
            |io, cx, mut plans| {
                io.read(cx, plans.pop().flatten().expect("required container plan"))
            },
        );
    }
    let Some(resolution) = find(cx, &request.name)? else {
        if text::offset(&request.mode).map_err(error)?.is_some() {
            return plans(
                cx,
                vec![(request.name.clone(), true)],
                ContainerIo { request, next },
                |mut io, cx, mut plans| {
                    io.request.name = plans.pop().flatten().expect("update destination").name;
                    if find(cx, &io.request.name)?.is_some() {
                        container_io(cx, io.request, io.next)
                    } else {
                        cx.storage_io_direct(io.request, io.next)
                    }
                },
            );
        }
        return cx.storage_io_direct(request, next);
    };
    let limit = service(cx)?.borrow().limits().max_read_bytes;
    let operation = match &request.io {
        tjs_core::storage::Io::ReadText | tjs_core::storage::Io::ReadBinary => Operation::Read,
        tjs_core::storage::Io::WriteText(source) => {
            text::encode(source, &request.mode, limit).map_err(error)?;
            Operation::Write
        }
        tjs_core::storage::Io::WriteBinary(bytes) => {
            if bytes.len() > limit {
                return Err(NativeError::Message("storage write exceeds byte budget"));
            }
            Operation::Write
        }
    };
    let operation = if text::offset(&request.mode).map_err(error)?.is_some() {
        Operation::Update
    } else {
        operation
    };
    resolution.start(cx, operation, Box::new(ContainerIo { request, next }))
}
struct ContainerIo {
    request: tjs_core::storage::Request,
    next: Box<dyn NativeContinuation>,
}
impl Trace for ContainerIo {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl NativeContinuation for ContainerIo {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.next.resume(cx, Value::Void)
    }
}

impl ContainerIo {
    fn read(self, cx: &mut NativeCx<'_>, plan: ReadPlan) -> NativeResult<NativeStep> {
        let offset = text::offset(&self.request.mode)
            .map_err(error)?
            .unwrap_or(0);
        let bytes = plan.read(offset).map_err(error)?;
        let value = if matches!(self.request.io, tjs_core::storage::Io::ReadText) {
            let limit = service(cx)?.borrow().limits().max_read_bytes;
            let text = text::decode(&bytes, &name::units("utf-8"), limit).map_err(error)?;
            Value::Str(cx.heap_mut().alloc_string(text))
        } else {
            Value::Octet(cx.heap_mut().alloc_octet(bytes))
        };
        self.next.resume(cx, value)
    }
}
/// Resolve a group in order (including optional sidecars) before handing owned
/// plans to the existing worker operation. No caller needs to replay its body.
pub fn plans<T: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    requests: Vec<(Vec<u16>, bool)>,
    state: T,
    next: fn(T, &mut NativeCx<'_>, Vec<Option<ReadPlan>>) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    PlanGroup {
        requests: requests.into_iter(),
        resolved: Vec::new(),
        state,
        next: PlansReady::Group(next),
        lookup: Default::default(),
        pending: None,
        search: None,
        candidates: std::collections::VecDeque::new(),
        first_only: false,
    }
    .advance(cx)
}
type FirstPlanReady<T> = fn(
    T,
    &mut NativeCx<'_>,
    Vec<Option<ReadPlan>>,
    krkr_assets::Lookup,
) -> NativeResult<NativeStep>;

/// Try alternatives in order and stop at the first existing resource. A group
/// shares ordinary lookup work, but never snapshots across a provider callback.
pub(super) fn first_plan<T: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    requests: Vec<Vec<u16>>,
    lookup: krkr_assets::Lookup,
    state: T,
    next: FirstPlanReady<T>,
) -> NativeResult<NativeStep> {
    PlanGroup {
        requests: requests
            .into_iter()
            .map(|path| (path, false))
            .collect::<Vec<_>>()
            .into_iter(),
        resolved: Vec::new(),
        state,
        next: PlansReady::First(next),
        lookup,
        pending: None,
        search: None,
        candidates: std::collections::VecDeque::new(),
        first_only: true,
    }
    .advance(cx)
}
enum PlansReady<T> {
    Group(fn(T, &mut NativeCx<'_>, Vec<Option<ReadPlan>>) -> NativeResult<NativeStep>),
    First(FirstPlanReady<T>),
}
struct PlanGroup<T> {
    requests: std::vec::IntoIter<(Vec<u16>, bool)>,
    resolved: Vec<Option<ReadPlan>>,
    state: T,
    next: PlansReady<T>,
    lookup: krkr_assets::Lookup,
    pending: Option<Vec<u16>>,
    search: Option<(Vec<u16>, bool)>,
    candidates: std::collections::VecDeque<Vec<u16>>,
    first_only: bool,
}
impl<T: Trace> Trace for PlanGroup<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.state.trace(visit);
    }
}
impl<T: Trace + 'static> PlanGroup<T> {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        // Never carry this snapshot across a managed provider's script call.
        let mut lookup = std::mem::take(&mut self.lookup);
        let context_timer = krkr_protocol::diagnostics::Timer::start();
        let vfs = service(cx)?;
        let managed = registry(cx.heap_mut())?;
        context_timer.report(|| "stage=storage-context".into());
        loop {
            if self.first_only && self.resolved.last().is_some_and(Option::is_some) {
                break;
            }
            if self.search.is_none() {
                let Some((path, required)) = self.requests.next() else {
                    break;
                };
                let search_timer = krkr_protocol::diagnostics::Timer::start();
                let search = vfs
                    .borrow_mut()
                    .search_in(&path, &mut lookup)
                    .map_err(error)?;
                search_timer.report(|| {
                    format!(
                        "stage=storage-search name={}",
                        String::from_utf16_lossy(&path)
                    )
                });
                match search {
                    Search::Found { plan, .. } => {
                        self.resolved.push(Some(plan));
                        continue;
                    }
                    Search::Candidates(candidates) => self.candidates = candidates.into(),
                }
                self.search = Some((path, required));
            }
            let Some(path) = self.candidates.pop_front() else {
                let (path, required) = self.search.take().expect("active storage search");
                if required {
                    return Err(NativeError::Detail(format!(
                        "storage not found: {}",
                        String::from_utf16_lossy(&path)
                    )));
                }
                self.resolved.push(None);
                continue;
            };
            let candidate_timer = krkr_protocol::diagnostics::Timer::start();
            if let Some(resolution) = find_candidate(cx, &managed, &path)? {
                self.pending = Some(resolution.path.clone());
                drop(lookup);
                return resolution.start(cx, Operation::Read, Box::new(self));
            }
            let mut vfs = vfs.borrow_mut();
            let plan = vfs.direct_plan_in(&path, &mut lookup).map_err(error)?;
            candidate_timer.report(|| {
                format!(
                    "stage=storage-candidate name={}",
                    String::from_utf16_lossy(&path)
                )
            });
            if let Some(plan) = plan {
                self.resolved.push(Some(plan));
                self.search = None;
                self.candidates.clear();
            }
        }
        match self.next {
            PlansReady::Group(next) => {
                drop(lookup);
                next(self.state, cx, self.resolved)
            }
            PlansReady::First(next) => next(self.state, cx, self.resolved, lookup),
        }
    }
}
impl<T: Trace + 'static> NativeContinuation for PlanGroup<T> {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let path = self.pending.take().expect("pending managed plan");
        if !matches!(value, Value::Void) {
            self.resolved.push(Some(read_plan(cx, path, value)?));
            self.search = None;
            self.candidates.clear();
        }
        self.advance(cx)
    }
}
impl ReadSource for Bytes {
    fn open(&self) -> krkr_assets::Result<Box<dyn Stream>> {
        Ok(Box::new(Cursor::new(self.0.clone())))
    }
}
/// Freeze just the file returned by this read operation, never its directory or
/// global object. Later resolutions observe subsequent script mutations.
pub fn read_plan(cx: &mut NativeCx<'_>, path: Vec<u16>, value: Value) -> NativeResult<ReadPlan> {
    let Value::Octet(id) = value else {
        return Err(NativeError::Detail(format!(
            "storage not found: {}",
            String::from_utf16_lossy(&path)
        )));
    };
    let limit = service(cx)?.borrow().limits().max_read_bytes;
    let bytes = cx.heap().octet(id)?;
    if bytes.len() > limit {
        return Err(NativeError::Message("managed file exceeds read budget"));
    }
    let bytes: Arc<[u8]> = Arc::from(bytes);
    Ok(ReadPlan::custom(
        path,
        bytes.len() as u64,
        limit,
        Arc::new(Bytes(bytes)),
    ))
}
