//! Ordered script events shared by timers, triggers and later window sources.
use slotmap::{SlotMap, new_key_type};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use tjs_core::{NativeError, NativeResult, ObjId, Value};

#[derive(Default)]
pub(crate) struct ActionOwner {
    function: Value,
    name: Option<Value>,
    event_type: Value,
}
impl tjs_core::Trace for ActionOwner {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.function);
        if let Some(name) = self.name {
            visit(name);
        }
        visit(self.event_type);
    }
}
impl ActionOwner {
    pub fn new(
        cx: &mut tjs_core::NativeCx<'_>,
        owner: Value,
        name: Option<Value>,
        event_type: &str,
    ) -> NativeResult<Self> {
        if !matches!(owner, Value::Obj(_)) {
            return Err(NativeError::Type("an action owner object"));
        }
        let name = match name.filter(|v| !matches!(v, Value::Void)) {
            Some(name) => {
                let Value::Str(id) = tjs_core::value::to_string(cx.heap_mut(), name)? else {
                    unreachable!()
                };
                tjs_core::string::c_string(cx.heap().string(id)?).to_vec()
            }
            None => "action".encode_utf16().collect(),
        };
        let name = (!name.is_empty()).then(|| Value::Str(cx.heap_mut().alloc_string(name)));
        let event_type = Value::Str(
            cx.heap_mut()
                .alloc_string(event_type.encode_utf16().collect::<Vec<_>>()),
        );
        Ok(Self {
            function: owner,
            name,
            event_type,
        })
    }
    pub fn invoke(&self, cx: &mut tjs_core::NativeCx<'_>) -> NativeResult<tjs_core::NativeStep> {
        self.invoke_with(cx, &[])
    }
    pub fn invoke_with(
        &self,
        cx: &mut tjs_core::NativeCx<'_>,
        fields: &[(&str, Value)],
    ) -> NativeResult<tjs_core::NativeStep> {
        use tjs_core::{NativeStep, ObjRef};
        if matches!(self.function, Value::Obj(ObjRef { object: None, .. })) {
            return Ok(NativeStep::Return(Value::Void));
        }
        let event = cx.heap_mut().alloc_dictionary();
        let target = Value::Obj(ObjRef::bound(cx.this()));
        // The event dictionary is fresh (scripts may retain it); names and the
        // immutable event type are retained once per action owner.
        let type_key = cx.heap_mut().intern(&b"type".map(u16::from));
        let target_key = cx.heap_mut().intern(&b"target".map(u16::from));
        cx.heap_mut().set_member(event, type_key, self.event_type)?;
        cx.heap_mut().set_member(event, target_key, target)?;
        for &(name, value) in fields {
            let key = cx.heap_mut().intern_str(name);
            cx.heap_mut().set_member(event, key, value)?;
        }
        let event = Value::Obj(ObjRef::bound(event));
        Ok(if let Some(key) = self.name {
            // Native action forwarding ignores a missing receiver method
            // (EventIntf.h); script getter/callback exceptions still propagate.
            NativeStep::GetOptional {
                object: self.function,
                key,
                continuation: Box::new(Action {
                    owner: self.function,
                    event,
                }),
            }
        } else {
            NativeStep::Call {
                function: self.function,
                arguments: vec![event],
                continuation: Box::new(Returned),
            }
        })
    }
}
struct Action {
    owner: Value,
    event: Value,
}
impl tjs_core::Trace for Action {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.owner);
        visit(self.event);
    }
}
impl tjs_core::NativeContinuation for Action {
    fn resume(
        self: Box<Self>,
        _: &mut tjs_core::NativeCx<'_>,
        mut function: Value,
    ) -> NativeResult<tjs_core::NativeStep> {
        use tjs_core::NativeStep;
        let Value::Obj(ref mut reference) = function else {
            return Ok(NativeStep::Return(Value::Void));
        };
        if reference.object.is_none() {
            return Ok(NativeStep::Return(Value::Void));
        }
        if reference.this.is_none()
            && let Value::Obj(owner) = self.owner
        {
            reference.this = owner.object;
        }
        Ok(NativeStep::Call {
            function,
            arguments: vec![self.event],
            continuation: Box::new(Returned),
        })
    }
}
struct Returned;
impl tjs_core::Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl tjs_core::NativeContinuation for Returned {
    fn resume(
        self: Box<Self>,
        _: &mut tjs_core::NativeCx<'_>,
        value: Value,
    ) -> NativeResult<tjs_core::NativeStep> {
        Ok(tjs_core::NativeStep::Return(value))
    }
}

new_key_type! { pub(crate) struct SourceId; }
pub(crate) type Shared = Rc<RefCell<Events>>;
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Video,
    Timer,
    Trigger,
    Window,
    Sound,
}
#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub source: SourceId,
    pub owner: ObjId,
    pub kind: Kind,
    pub priority: usize,
    epoch: u64,
}
struct Source {
    owner: ObjId,
    kind: Kind,
    capacity: usize,
    pending: usize,
    reserved: usize,
}
pub(crate) struct Events {
    sources: SlotMap<SourceId, Source>,
    queues: [VecDeque<Event>; 3],
    limit: usize,
    reserved: usize,
    epoch: u64,
    round: Option<u64>,
}
impl Events {
    pub fn new(limit: usize) -> Self {
        Self {
            sources: SlotMap::with_key(),
            queues: std::array::from_fn(|_| VecDeque::new()),
            limit,
            reserved: 0,
            epoch: 0,
            round: None,
        }
    }
    fn budget(&self, total: usize) -> NativeResult<()> {
        if total > self.limit {
            Err(tjs_core::NativeError::Message(
                "event capacity exceeds host budget",
            ))
        } else {
            Ok(())
        }
    }
    pub fn insert(&mut self, owner: ObjId, kind: Kind, capacity: usize) -> NativeResult<SourceId> {
        self.budget(self.reserved.saturating_add(capacity))?;
        self.reserved += capacity;
        Ok(self.sources.insert(Source {
            owner,
            kind,
            capacity,
            pending: 0,
            reserved: capacity,
        }))
    }
    pub fn pending(&self, id: SourceId) -> usize {
        self.sources[id].pending
    }
    pub fn capacity(&mut self, id: SourceId, capacity: usize) -> NativeResult<()> {
        let source = &self.sources[id];
        let reserved = capacity.max(source.pending);
        let total = self.reserved - source.reserved + reserved;
        self.budget(total)?;
        let source = &mut self.sources[id];
        source.capacity = capacity;
        source.reserved = reserved;
        self.reserved = total;
        Ok(())
    }
    pub fn post(&mut self, id: SourceId, count: usize, mode: i32) -> NativeResult<()> {
        let source = &self.sources[id];
        let pending = source
            .pending
            .checked_add(count)
            .ok_or(NativeError::Message("event count overflow"))?;
        let reserved = source.capacity.max(pending);
        let total = self.reserved - source.reserved + reserved;
        self.budget(total)?;
        let source = &mut self.sources[id];
        self.reserved = total;
        source.reserved = reserved;
        source.pending = pending;
        let priority = match mode {
            1 => 0,
            2 => 2,
            _ => 1,
        };
        self.queues[priority].extend((0..count).map(|_| Event {
            source: id,
            owner: source.owner,
            kind: source.kind,
            priority,
            epoch: self.epoch,
        }));
        Ok(())
    }
    pub fn cancel(&mut self, id: SourceId) {
        let source = &mut self.sources[id];
        if source.pending == 0 {
            return;
        }
        for queue in &mut self.queues {
            queue.retain(|e| e.source != id);
        }
        source.pending = 0;
        self.reserved -= source.reserved - source.capacity;
        source.reserved = source.capacity;
    }
    pub fn remove(&mut self, id: SourceId) {
        self.cancel(id);
        if let Some(source) = self.sources.remove(id) {
            self.reserved -= source.reserved;
        }
    }
    pub fn peek(&self) -> Option<Event> {
        self.queues.iter().find_map(|q| q.front().copied())
    }
    /// A callback's new normal/idle events belong to the next delivery round.
    /// This lets continuous handlers run even when a trigger reposts itself.
    pub fn peek_round(&mut self) -> Option<Event> {
        if self.round.is_none()
            || self.queues[0]
                .front()
                .is_some_and(|e| Some(e.epoch) > self.round)
        {
            // A newly posted exclusive event interrupts the remaining round.
            self.round = Some(self.epoch);
            self.epoch += 1;
        }
        let round = self.round.expect("started round");
        self.queues
            .iter()
            .find_map(|queue| queue.front().copied().filter(|event| event.epoch <= round))
    }
    pub fn finish_round(&mut self) {
        self.round = None;
    }
    pub fn pop(&mut self, priority: usize) -> Option<Event> {
        let event = self.queues[priority].pop_front()?;
        let source = &mut self.sources[event.source];
        source.pending -= 1;
        let reserved = source.capacity.max(source.pending);
        self.reserved -= source.reserved - reserved;
        source.reserved = reserved;
        Some(event)
    }
    pub fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        self.sources
            .values()
            .filter(|s| s.pending != 0)
            .map(|s| Value::Obj(s.owner.into()))
    }
    pub fn clear(&mut self) {
        self.round = None;
        self.epoch = 0;
        for queue in &mut self.queues {
            queue.clear();
        }
        for source in self.sources.values_mut() {
            source.pending = 0;
            source.reserved = source.capacity;
        }
        self.reserved = self.sources.values().map(|s| s.reserved).sum();
    }
}
