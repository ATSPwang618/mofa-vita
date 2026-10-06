use super::*;

impl Heap {
    pub fn set_call_missing(&mut self, id: ObjId) -> Result<(), HeapError> {
        let object = self.object_mut(id)?;
        object.ensure_valid()?;
        object.missing.get_or_insert_with(std::rc::Weak::new);
        Ok(())
    }

    pub(crate) fn calls_missing(&self, id: ObjId) -> Result<bool, HeapError> {
        Ok(self
            .object(id)?
            .missing
            .as_ref()
            .is_some_and(|guard| guard.strong_count() == 0))
    }

    pub(crate) fn enter_missing(&mut self, id: ObjId) -> Result<Rc<()>, HeapError> {
        let guard = Rc::new(());
        self.object_mut(id)?.missing = Some(Rc::downgrade(&guard));
        Ok(guard)
    }

    // A heap-owned value replaces the reference implementation's borrowed
    // stack pointer. A script may retain the property after missing returns.
    pub(crate) fn missing_property(&mut self, value: Value) -> Value {
        let id = self.alloc_record(ObjectData::NativeProperty {
            descriptor: &CELL,
            get: None,
            set: None,
        });
        self.object_mut(id)
            .expect("new property")
            .native
            .push(Some(Box::new(value)));
        Value::Obj(ObjRef::bound(id))
    }

    pub(crate) fn missing_value(&mut self, id: ObjId) -> Result<Value, NativeError> {
        self.with_native_state::<Value, _>(id, |value| *value)
    }
}

static CELL: NativeProperty = NativeProperty {
    name: "",
    doc: "",
    hidden: false,
    class_only: false,
    get: Some(NativeCallable::Leaf(|cx, _| {
        let id = cx.this();
        cx.heap_mut().missing_value(id)
    })),
    set: Some(NativeCallable::Leaf(|cx, args| {
        let id = cx.this();
        cx.heap_mut()
            .with_native_state::<Value, _>(id, |value| *value = args[0])?;
        Ok(Value::Void)
    })),
};
