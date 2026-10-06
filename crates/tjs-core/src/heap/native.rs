use super::*;

impl Heap {
    pub(crate) fn native_index(
        &self,
        object: ObjId,
    ) -> Result<Option<crate::native::NativeIndex>, HeapError> {
        Ok(self
            .object(object)?
            .native
            .iter()
            .flatten()
            .find_map(|state| {
                state
                    .any()
                    .downcast_ref::<crate::native::NativeIndex>()
                    .copied()
            }))
    }
    /// Register intrinsic cleanup for an attached native state without creating
    /// a script-visible class. Runs in reverse state order through the VM's
    /// existing invalidation path, including explicit invalidate and GC.
    pub fn register_state_invalidator<T: Trace + 'static>(&mut self, call: NativeCallable) {
        self.native_invalidators
            .entry(std::any::TypeId::of::<T>())
            .or_insert(call);
    }
    /// A host/plugin free function, executed through the ordinary native VM path.
    pub fn alloc_native_function(&mut self, call: NativeCallable) -> ObjId {
        self.alloc_record(ObjectData::NativeFunction(call))
    }
    /// An unbound native accessor, using the same VM dispatch as class properties.
    /// The caller owns installation and the member slot's visibility/static flags.
    pub fn alloc_native_property(&mut self, property: &'static NativeProperty) -> ObjId {
        let mut accessor = |call| match call {
            Some(call @ NativeCallable::Resumable(_)) => Some(self.alloc_native_function(call)),
            _ => None,
        };
        let get = accessor(property.get);
        let set = accessor(property.set);
        self.alloc_record(ObjectData::NativeProperty {
            descriptor: property,
            get,
            set,
        })
    }
    /// Install shared native descriptions. The registry keeps classes alive for
    /// this heap; instances keep a member snapshot and their typed Rust states.
    pub fn register_class(&mut self, class: &'static NativeClass) -> Result<ObjId, NativeError> {
        self.register_class_variant(class.name, class)
    }
    /// Keep a plugin's same-named class separate from the engine class. The
    /// registry identity is private to the host; script names and constructors
    /// still come from the descriptor, and existing instances keep their state.
    pub fn register_class_variant(
        &mut self,
        identity: &'static str,
        class: &'static NativeClass,
    ) -> Result<ObjId, NativeError> {
        if let Some(&id) = self.native_classes.get(identity) {
            if std::ptr::eq(self.native_class(id)?, class) {
                return Ok(id);
            }
            return Err(NativeError::Message(
                "native class name is already registered",
            ));
        }
        if class.storage != NativeStorage::Object && class.literal.is_none() {
            return Err(NativeError::Message(
                "intrinsic class needs a literal factory",
            ));
        }
        let id = self.alloc_record(ObjectData::NativeClass(class));
        for method in class.methods {
            let name = self.intern_str(method.name);
            let value = self.alloc_record(ObjectData::NativeFunction(method.call));
            self.set_member_flags(
                id,
                name,
                Value::Obj(value.into()),
                method.hidden,
                method.class_only,
            )?;
        }
        for property in class.properties {
            let name = self.intern_str(property.name);
            let value = self.alloc_native_property(property);
            self.set_member_flags(
                id,
                name,
                Value::Obj(ObjRef {
                    object: Some(value),
                    this: property.class_only.then_some(id),
                }),
                property.hidden,
                property.class_only,
            )?;
        }
        let constructor = self.alloc_record(ObjectData::NativeConstructor(id));
        let name = self.intern_str(class.name);
        self.set_member_flags(
            id,
            name,
            Value::Obj(constructor.into()),
            false,
            class.constructor_class_only,
        )?;
        self.native_classes.insert(identity, id);
        if identity != class.name {
            self.nested_classes.insert(id);
        }
        if let Some((kind, invalidate)) = class.invalidate {
            self.native_invalidators.insert(kind(), invalidate);
        }
        match class.storage {
            NativeStorage::Array => self.array_class = Some(id),
            NativeStorage::Dictionary => self.dictionary_class = Some(id),
            NativeStorage::Object => {}
        }
        Ok(id)
    }

    /// Remove a class from default globals after attaching it under an owner.
    /// Registration remains idempotent and retains the class metadata.
    pub fn nest_class(
        &mut self,
        owner: ObjId,
        name: &str,
        class: ObjId,
    ) -> Result<(), NativeError> {
        let symbol = self.intern_str(name);
        self.set_member(owner, symbol, Value::Obj(class.into()))?;
        self.nested_classes.insert(class);
        Ok(())
    }

    pub fn alloc_global(&mut self) -> ObjId {
        let id = self.alloc_object();
        let classes: Vec<_> = self
            .native_classes
            .iter()
            .filter(|(_, id)| !self.nested_classes.contains(id))
            .map(|(&name, &id)| (name, id))
            .collect();
        for (name, class) in classes {
            let name = self.intern_str(name);
            self.set_member(id, name, Value::Obj(class.into()))
                .expect("new global");
        }
        id
    }

    pub fn registered_class(&self, name: &str) -> Option<ObjId> {
        self.native_classes.get(name).copied()
    }

    pub fn native_class(&self, id: ObjId) -> Result<&'static NativeClass, NativeError> {
        match self.object(id)?.data {
            ObjectData::NativeClass(class) => Ok(class),
            _ => Err(NativeError::This),
        }
    }

    pub fn alloc_native<T: Trace + 'static>(
        &mut self,
        class: ObjId,
        state: T,
    ) -> Result<ObjId, NativeError> {
        let id = self.alloc_native_object(class)?;
        self.initialize_native_state(id, state)?;
        self.attach_native_class(class, id)?;
        Ok(id)
    }

    pub(crate) fn alloc_native_object(&mut self, class: ObjId) -> Result<ObjId, NativeError> {
        let data = match self.native_class(class)?.storage {
            NativeStorage::Object => ObjectData::Plain,
            NativeStorage::Array => ObjectData::Array(Vec::new()),
            NativeStorage::Dictionary => ObjectData::Dictionary,
        };
        Ok(self.alloc_record(data))
    }

    fn attach_native_class(&mut self, class: ObjId, instance: ObjId) -> Result<(), NativeError> {
        let name = self.native_class(class)?.name;
        let name = self.intern_str(name);
        self.add_class_name(instance, name)?;
        self.copy_native_members(class, instance)?;
        Ok(())
    }

    pub(crate) fn initialize_native(
        &mut self,
        class: ObjId,
        instance: ObjId,
    ) -> Result<(), NativeError> {
        self.ensure_valid(instance)?;
        let definition = self.native_class(class)?;
        match definition.storage {
            NativeStorage::Array if self.object(instance)?.kind() != ObjectKind::Array => {
                self.initialize_native_default::<arrays::InheritedArray>(instance)?;
            }
            NativeStorage::Dictionary
                if self.object(instance)?.kind() != ObjectKind::Dictionary =>
            {
                self.initialize_native_default::<arrays::InheritedDictionary>(instance)?;
            }
            _ => {}
        }
        (definition.initialize)(self, instance)?;
        self.attach_native_class(class, instance)
    }

    /// Repeated base initialization preserves its state and avoids constructing
    /// a second Rust payload (including any resources owned by Default).
    pub fn initialize_native_default<T: Trace + Default + 'static>(
        &mut self,
        object: ObjId,
    ) -> Result<(), NativeError> {
        if self
            .valid_object_mut(object)?
            .native
            .iter()
            .flatten()
            .any(|state| state.any().is::<T>())
        {
            return Ok(());
        }
        self.initialize_native_state(object, T::default())
    }

    /// A native base allocates state once per Rust type, even in a diamond.
    pub fn initialize_native_state<T: Trace + 'static>(
        &mut self,
        object: ObjId,
        state: T,
    ) -> Result<(), NativeError> {
        let record = self.valid_object_mut(object)?;
        if !record
            .native
            .iter()
            .flatten()
            .any(|state| state.any().is::<T>())
        {
            record.native.push(Some(Box::new(state)));
            self.allocation_debt = self
                .allocation_debt
                .saturating_add(size_of::<T>() + size_of::<Box<dyn NativeState>>());
        }
        Ok(())
    }

    pub(crate) fn replace_native_state<T: Trace + 'static>(
        &mut self,
        object: ObjId,
        state: T,
    ) -> Result<(), NativeError> {
        self.with_native_state::<T, _>(object, |slot| *slot = state)
    }

    pub(crate) fn take_native_state<T: 'static>(
        &mut self,
        id: ObjId,
    ) -> Result<(usize, Box<dyn NativeState>), NativeError> {
        let states = &mut self.valid_object_mut(id)?.native;
        let index = states
            .iter()
            .position(|state| state.as_ref().is_some_and(|state| state.any().is::<T>()))
            .ok_or(NativeError::This)?;
        Ok((index, states[index].take().expect("matching native state")))
    }

    pub(crate) fn restore_native_state(
        &mut self,
        id: ObjId,
        index: usize,
        state: Box<dyn NativeState>,
    ) {
        self.object_mut(id)
            .expect("no collection during native leaf")
            .native[index] = Some(state);
    }

    pub(crate) fn is_native_constructor(&self, id: ObjId) -> Result<bool, HeapError> {
        Ok(matches!(
            self.object(id)?.data,
            ObjectData::NativeConstructor(_)
        ))
    }

    pub(crate) fn native_callable(
        &self,
        id: ObjId,
        construct: bool,
    ) -> Result<Option<NativeCallable>, HeapError> {
        Ok(match self.object(id)?.data {
            ObjectData::NativeFunction(call) if !construct => Some(call),
            ObjectData::NativeConstructor(class) if !construct => {
                let ObjectData::NativeClass(class) = self.object(class)?.data else {
                    unreachable!("native constructor owner")
                };
                Some(class.constructor)
            }
            _ => None,
        })
    }

    pub(crate) fn native_property(
        &self,
        value: Value,
    ) -> Result<Option<&'static NativeProperty>, HeapError> {
        if let Value::Obj(reference) = value {
            if let Some(id) = reference.object {
                if let ObjectData::NativeProperty { descriptor, .. } = self.object(id)?.data {
                    return Ok(Some(descriptor));
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn native_accessor(
        &self,
        value: Value,
        setter: bool,
    ) -> Result<Option<ObjId>, HeapError> {
        if let Value::Obj(ObjRef {
            object: Some(id), ..
        }) = value
        {
            if let ObjectData::NativeProperty { get, set, .. } = self.object(id)?.data {
                return Ok(if setter { set } else { get });
            }
        }
        Ok(None)
    }

    /// Initialize per-heap class state once; repeated installation preserves it.
    pub fn initialize_class_state<T: Trace + Default + 'static>(
        &mut self,
        class: ObjId,
    ) -> Result<(), NativeError> {
        self.native_class(class)?;
        self.initialize_native_default::<T>(class)
    }

    /// Inspect typed state without marking the object as modified.
    ///
    /// The closure must not change traced edges through interior mutability.
    /// Use `with_native_state` for changes, including those made through `Rc`
    /// or `RefCell` fields. This distinction lets read-only getters run between
    /// incremental collection slices without repeatedly retracing their owner.
    pub fn inspect_native_state<T: Trace + 'static, R>(
        &self,
        object: ObjId,
        f: impl FnOnce(&T) -> R,
    ) -> Result<R, NativeError> {
        let record = self.object(object)?;
        record.ensure_valid()?;
        let state = record
            .native
            .iter()
            .flatten()
            .find_map(|state| state.any().downcast_ref::<T>())
            .ok_or(NativeError::This)?;
        Ok(f(state))
    }

    /// Borrow typed state without exposing a managed reference beyond the closure.
    pub fn with_native_state<T: Trace + 'static, R>(
        &mut self,
        object: ObjId,
        f: impl FnOnce(&mut T) -> R,
    ) -> Result<R, NativeError> {
        let native = self
            .valid_object_mut(object)?
            .native
            .iter_mut()
            .flatten()
            .find(|state| state.any().is::<T>())
            .ok_or(NativeError::This)?;
        let state = native.any_mut().downcast_mut::<T>().expect("type checked");
        Ok(f(state))
    }
}
