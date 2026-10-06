//! Raw container copying on the ordinary native continuation stack.
use std::collections::HashSet;

use crate::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
};
use tjs_core::{ObjRef, ObjectKind};

fn source(value: Value) -> NativeResult<ObjId> {
    let Value::Obj(reference) = value else {
        return Err(NativeError::Type("an object"));
    };
    reference
        .this
        .or(reference.object)
        .ok_or(NativeError::Message("member access on null"))
}

// Native array contents after invalidation require the separate lifetime
// contract: ordinary member enumeration must not pretend they were preserved.
fn kind(heap: &Heap, object: ObjId) -> NativeResult<ObjectKind> {
    match heap.container_kind(object) {
        Ok(kind) => Ok(kind),
        Err(tjs_core::HeapError::InvalidObject)
            if heap.object(object)?.kind() != ObjectKind::Array =>
        {
            Ok(heap.object(object)?.kind())
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn assign(
    cx: &mut NativeCx<'_>,
    value: Value,
    clear: bool,
    deep: bool,
) -> NativeResult<NativeStep> {
    let target = cx.this();
    let array = cx.heap().container_kind(target)? == ObjectKind::Array;
    // Array clears even before converting the source closure. Dictionary
    // converts it (and checks deep-copy kind) before clearing.
    if array {
        cx.heap_mut().array_resize(target, 0)?;
    }
    let source = source(value)?;
    if deep && kind(cx.heap(), source)? != cx.heap().container_kind(target)? {
        return Err(NativeError::Type("a container of the same kind"));
    }
    if !array && clear {
        cx.heap_mut().clear_members(target)?;
    }
    let frame = Frame::new(cx.heap(), target, source, array)?;
    Copy {
        frames: vec![frame],
        ancestors: HashSet::from([source]),
        deep,
    }
    .advance(cx)
}

struct Entry {
    // Owned names survive source deletion and symbol collection in a callback.
    key: Option<Vec<u16>>,
    value: Value,
    class_only: bool,
}
impl Trace for Entry {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.value.trace(visit);
    }
}

enum Input {
    Array { index: usize },
    Members(std::vec::IntoIter<Entry>),
}
struct Frame {
    target: ObjId,
    source: ObjId,
    array: bool,
    input: Input,
    // Dictionary publishes a child only after its recursive copy completes.
    after: Option<Entry>,
}
impl Frame {
    fn new(heap: &Heap, target: ObjId, source: ObjId, array: bool) -> NativeResult<Self> {
        let input = if kind(heap, source)? == ObjectKind::Array {
            Input::Array { index: 0 }
        } else if !heap.is_valid(source)? {
            // EnumMembers returns E_INVALIDOBJECT; assignment ignores that
            // status, after clearing the destination if requested.
            Input::Members(Vec::new().into_iter())
        } else {
            // Capture the hash table once, never restart enumeration using
            // nth(index). Member copying itself proceeds in slices.
            let entries = heap
                .members_with_flags(source)?
                .map(|(key, value, class_only)| {
                    Ok(Entry {
                        key: Some(heap.symbol(key)?.to_vec()),
                        value,
                        class_only,
                    })
                })
                .collect::<NativeResult<Vec<_>>>()?;
            Input::Members(entries.into_iter())
        };
        Ok(Self {
            target,
            source,
            array,
            input,
            after: None,
        })
    }

    fn next(&mut self, heap: &Heap) -> NativeResult<Option<Entry>> {
        match &mut self.input {
            Input::Members(entries) => Ok(entries.next()),
            Input::Array { index } => {
                let values = heap.array(self.source)?;
                let Some(&value) = values.get(*index) else {
                    return Ok(None);
                };
                *index += 1;
                if self.array {
                    return Ok(Some(Entry {
                        key: None,
                        value,
                        class_only: false,
                    }));
                }
                // AsStringNoAddRef is not a general conversion. An odd final
                // key is validated before checking for the associated value.
                let key = match value {
                    Value::Void => None,
                    Value::Str(id) => {
                        let units = heap.string(id)?;
                        if units.is_empty() {
                            None
                        } else {
                            Some(units.to_vec())
                        }
                    }
                    _ => return Err(NativeError::Type("a string dictionary key")),
                };
                let Some(&value) = values.get(*index) else {
                    return Ok(None);
                };
                *index += 1;
                Ok(Some(Entry {
                    key,
                    value,
                    class_only: false,
                }))
            }
        }
    }
}
impl Trace for Frame {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.target.trace(visit);
        self.source.trace(visit);
        self.after.trace(visit);
        if let Input::Members(entries) = &self.input {
            for entry in entries.as_slice() {
                entry.trace(visit);
            }
        }
    }
}

struct Copy {
    frames: Vec<Frame>,
    // Only the active path is a cycle. Shared siblings get independent copies.
    ancestors: HashSet<ObjId>,
    deep: bool,
}
impl Trace for Copy {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.frames.trace(visit);
    }
}
impl NativeContinuation for Copy {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx)
    }
}
struct Write {
    target: ObjId,
    key: Value,
    value: Value,
    class_only: bool,
}
impl Write {
    fn suspend(self, copy: Copy) -> NativeStep {
        NativeStep::CopyMember {
            object: Value::Obj(ObjRef::bound(self.target)),
            key: self.key,
            value: self.value,
            class_only: self.class_only,
            continuation: Box::new(copy),
        }
    }
}

impl Copy {
    fn store(
        &mut self,
        cx: &mut NativeCx<'_>,
        target: ObjId,
        entry: Entry,
    ) -> NativeResult<Option<Write>> {
        let key = Value::Str(
            cx.heap_mut()
                .alloc_string(entry.key.expect("dictionary entry")),
        );
        let class_only = !self.deep && entry.class_only;
        if cx.try_copy_member(target, key, entry.value, class_only)? {
            Ok(None)
        } else {
            Ok(Some(Write {
                target,
                key,
                value: entry.value,
                class_only,
            }))
        }
    }

    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        // No extra copy of an entire array buffer. Continue does not open the
        // event gate or turn this synchronous API into an asynchronous one.
        for _ in 0..64 {
            let Some(frame) = self.frames.last_mut() else {
                return Ok(NativeStep::Return(Value::Void));
            };
            let target = frame.target;
            if let Some(entry) = frame.after.take() {
                if let Some(write) = self.store(cx, target, entry)? {
                    return Ok(write.suspend(self));
                }
                continue;
            }
            let Some(mut entry) = frame.next(cx.heap())? else {
                self.ancestors.remove(&frame.source);
                self.frames.pop();
                continue;
            };
            let array = frame.array;
            if !array && entry.key.is_none() {
                // Null/empty names fail PropSet; the reference ignores its
                // returned status and continues with the next pair.
                continue;
            }
            if array && let Some(key) = entry.key.take() {
                let key = Value::Str(cx.heap_mut().alloc_string(key));
                cx.heap_mut().array_push(target, key)?;
            }
            if self.deep
                && let Value::Obj(ObjRef {
                    object: Some(object),
                    ..
                }) = entry.value
            {
                let kind = kind(cx.heap(), object)?;
                if matches!(kind, ObjectKind::Array | ObjectKind::Dictionary) {
                    if self.ancestors.contains(&object) {
                        entry.value = Value::Obj(ObjRef::default());
                    } else {
                        let child_array = kind == ObjectKind::Array;
                        let child = if child_array {
                            cx.heap_mut().alloc_array()
                        } else {
                            cx.heap_mut().alloc_dictionary()
                        };
                        entry.value = Value::Obj(ObjRef::bound(child));
                        if array {
                            // Array publishes before descending.
                            cx.heap_mut().array_push(target, entry.value)?;
                        } else {
                            frame.after = Some(entry);
                        }
                        self.frames
                            .push(Frame::new(cx.heap(), child, object, child_array)?);
                        self.ancestors.insert(object);
                        continue;
                    }
                }
            }
            if array {
                cx.heap_mut().array_push(target, entry.value)?;
            } else if let Some(write) = self.store(cx, target, entry)? {
                return Ok(write.suspend(self));
            }
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
}
