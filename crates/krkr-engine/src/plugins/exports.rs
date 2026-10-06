use super::Context;
use tjs_core::{NativeCallable, NativeProperty, NativeResult, ObjId, ObjRef, Trace, Value};

struct Slot {
    owner: ObjId,
    name: &'static str,
    value: Value,
    previous: Option<(Value, bool, bool)>,
}

/// Provider-owned exports retain both the installed value and the previous slot.
/// Unlink restores only slots still owned by this provider. Code remains valid
/// for existing closures/instances after the names have been released.
#[derive(Default)]
pub struct Exports {
    slots: Vec<Slot>,
}
impl Trace for Exports {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for slot in &self.slots {
            visit(Value::Obj(slot.owner.into()));
            visit(slot.value);
            if let Some((previous, _, _)) = slot.previous {
                visit(previous);
            }
        }
    }
}
impl Exports {
    pub fn value(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        name: &'static str,
        value: Value,
    ) -> NativeResult<()> {
        self.value_with_flags(cx, owner, name, value, false, false)
    }
    pub fn value_with_flags(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        name: &'static str,
        value: Value,
        hidden: bool,
        class_only: bool,
    ) -> NativeResult<()> {
        let key = cx.heap.intern_str(name);
        let previous = cx.member(owner, key)?;
        cx.export_member_with_flags(owner, name, value, hidden, class_only)?;
        self.slots.push(Slot {
            owner,
            name,
            value,
            previous,
        });
        Ok(())
    }
    pub fn function(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        name: &'static str,
        call: NativeCallable,
    ) -> NativeResult<()> {
        let function = cx.heap.alloc_native_function(call);
        self.value(cx, owner, name, Value::Obj(function.into()))
    }
    /// Bind a method to a managed service receiver retained by the closure.
    pub fn bound_function(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        name: &'static str,
        call: NativeCallable,
        receiver: ObjId,
    ) -> NativeResult<()> {
        let function = cx.heap.alloc_native_function(call);
        self.value(
            cx,
            owner,
            name,
            Value::Obj(ObjRef {
                object: Some(function),
                this: Some(receiver),
            }),
        )
    }
    /// Each method owns its capture. Retained methods survive unlink/relink
    /// with their original captured API instead of reading a new global state.
    pub fn captured_function<S: Trace + 'static>(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        name: &'static str,
        call: NativeCallable,
        state: S,
        class_only: bool,
    ) -> NativeResult<()> {
        let function = cx.heap.alloc_native_function(call);
        cx.heap.initialize_native_state(function, state)?;
        self.value_with_flags(
            cx,
            owner,
            name,
            Value::Obj(function.into()),
            false,
            class_only,
        )
    }
    pub fn property(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        property: &'static NativeProperty,
    ) -> NativeResult<()> {
        let object = cx.heap.alloc_native_property(property);
        self.value_with_flags(
            cx,
            owner,
            property.name,
            Value::Obj(ObjRef {
                object: Some(object),
                this: property.class_only.then_some(owner),
            }),
            property.hidden,
            property.class_only,
        )
    }
    pub fn bound_property(
        &mut self,
        cx: &mut Context<'_>,
        owner: ObjId,
        property: &'static NativeProperty,
        receiver: ObjId,
    ) -> NativeResult<()> {
        let object = cx.heap.alloc_native_property(property);
        self.value_with_flags(
            cx,
            owner,
            property.name,
            Value::Obj(ObjRef {
                object: Some(object),
                this: Some(receiver),
            }),
            property.hidden,
            property.class_only,
        )
    }
    pub fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        for slot in self.slots.iter().rev() {
            let (owner, name) = (slot.owner, slot.name);
            let key = cx.heap.intern_str(name);
            // Ordinary script assignment can change slot flags while invoking
            // this same setter. The installed accessor identity owns the export.
            if !cx.heap.is_valid(owner)? {
                continue;
            }
            if let Some((value, _, _)) = cx.member(owner, key)?
                && tjs_core::value::strict_equal(cx.heap, value, slot.value)?
            {
                if let Some((value, hidden, class_only)) = slot.previous {
                    cx.export_member_with_flags(owner, name, value, hidden, class_only)?;
                } else {
                    cx.remove_member(owner, name)?;
                }
            }
        }
        self.slots.clear();
        Ok(true)
    }
}
