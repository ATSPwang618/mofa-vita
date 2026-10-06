use super::*;
#[cfg(test)]
#[path = "../../tests/internal/member_writes.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct Member {
    pub value: Value,
    pub hidden: bool,
    pub class_only: bool,
    // Native copies bind previously unbound object closures to their instance.
    pub bind: bool,
}
pub(super) type Table = FxHashMap<SymbolId, Member>;

impl Member {
    fn value(self, owner: ObjId) -> Value {
        let mut value = self.value;
        if self.bind {
            if let Value::Obj(reference) = &mut value {
                reference.this = reference.this.or(Some(owner));
            }
        }
        value
    }
}

impl ObjRecord {
    pub(super) fn entries(&self) -> impl Iterator<Item = (SymbolId, Member)> + '_ {
        self.members
            .iter()
            .filter_map(|(&key, &entry)| entry.map(|entry| (key, entry)))
            .chain(
                self.shared_members
                    .iter()
                    .flat_map(|table| table.iter())
                    .filter(|(key, _)| !self.members.contains_key(key))
                    .map(|(&key, &entry)| (key, entry)),
            )
    }
}

impl Heap {
    fn stored_member(&self, id: ObjId, name: SymbolId) -> Result<Option<Member>, HeapError> {
        let object = self.object(id)?;
        Ok(object.members.get(&name).copied().unwrap_or_else(|| {
            object
                .shared_members
                .as_ref()
                .and_then(|table| table.get(&name).copied())
        }))
    }

    pub(crate) fn set_member_attributes(
        &mut self,
        id: ObjId,
        name: SymbolId,
        hidden: bool,
        class_only: bool,
    ) -> Result<(), HeapError> {
        if let Some(entry) = self
            .stored_member(id, name)?
            .filter(|entry| entry.hidden != hidden || entry.class_only != class_only)
        {
            self.set_member_flags(id, name, entry.value(id), hidden, class_only)?;
        }
        Ok(())
    }

    pub(crate) fn update_member_value(
        &mut self,
        id: ObjId,
        name: SymbolId,
        value: Value,
    ) -> Result<(), HeapError> {
        let entry = self.stored_member(id, name)?;
        self.set_member_flags(
            id,
            name,
            value,
            entry.is_some_and(|e| e.hidden),
            entry.is_some_and(|e| e.class_only),
        )?;
        Ok(())
    }

    pub fn member(&self, id: ObjId, name: SymbolId) -> Result<Option<Value>, HeapError> {
        self.lookup_member(id, name)
    }

    /// Inspect one raw slot, including hidden/static flags, without invoking an accessor.
    pub fn member_with_flags(
        &self,
        id: ObjId,
        name: SymbolId,
    ) -> Result<Option<(Value, bool, bool)>, HeapError> {
        Ok(self
            .stored_member(id, name)?
            .map(|entry| (entry.value(id), entry.hidden, entry.class_only)))
    }

    pub(crate) fn lookup_member(
        &self,
        id: ObjId,
        name: SymbolId,
    ) -> Result<Option<Value>, HeapError> {
        let object = self.object(id)?;
        let entry = object.members.get(&name).copied().unwrap_or_else(|| {
            object
                .shared_members
                .as_ref()
                .and_then(|table| table.get(&name).copied())
        });
        Ok(entry.map(|entry| entry.value(id)))
    }

    /// Observable raw-value enumeration: hidden members are omitted, getters are
    /// not called, and inherited native closures retain the instance context.
    pub fn members(
        &self,
        id: ObjId,
    ) -> Result<impl Iterator<Item = (SymbolId, Value)> + '_, HeapError> {
        let object = self.object(id)?;
        object.ensure_valid()?;
        Ok(object
            .entries()
            .filter(|(_, entry)| !entry.hidden)
            .map(move |(key, entry)| (key, entry.value(id))))
    }

    /// Raw enumeration including static slot flags, without invoking getters.
    pub fn members_with_flags(
        &self,
        id: ObjId,
    ) -> Result<impl Iterator<Item = (SymbolId, Value, bool)> + '_, HeapError> {
        let object = self.object(id)?;
        object.ensure_valid()?;
        Ok(object
            .entries()
            .filter(|(_, entry)| !entry.hidden)
            .map(move |(key, entry)| (key, entry.value(id), entry.class_only)))
    }

    /// Inspect all raw slots, including hidden members. Native enumeration
    /// adapters apply their own reference flag rules without invoking getters.
    pub fn all_members_with_flags(
        &self,
        id: ObjId,
    ) -> Result<impl Iterator<Item = (SymbolId, Value, bool, bool)> + '_, HeapError> {
        let object = self.object(id)?;
        object.ensure_valid()?;
        Ok(object
            .entries()
            .map(move |(key, entry)| (key, entry.value(id), entry.hidden, entry.class_only)))
    }

    pub(crate) fn delete_member(&mut self, id: ObjId, name: SymbolId) -> Result<bool, HeapError> {
        Ok(self.remove_member(id, name)?.is_some())
    }

    pub fn clear_members(&mut self, id: ObjId) -> Result<(), HeapError> {
        let object = self.object_mut_unbarriered(id)?;
        object.members.clear();
        object.shared_members = None;
        if std::mem::take(&mut object.native_snapshot) {
            self.native_snapshots.remove(&id);
        }
        Ok(())
    }

    pub fn set_member(
        &mut self,
        id: ObjId,
        name: SymbolId,
        value: Value,
    ) -> Result<Option<Value>, HeapError> {
        self.set_member_flags(id, name, value, false, false)
    }

    /// Flags belong to the member slot, not to the function/property value.
    pub fn set_member_flags(
        &mut self,
        id: ObjId,
        name: SymbolId,
        value: Value,
        hidden: bool,
        class_only: bool,
    ) -> Result<Option<Value>, HeapError> {
        self.symbol(name)?;
        self.object(id)?;
        self.edge_barrier(id, [value]);
        self.symbol_barrier(id, name);
        let entry = Member {
            value,
            hidden,
            class_only,
            bind: false,
        };
        let object = self.object_mut_unbarriered(id)?;
        let previous = object.members.insert(name, Some(entry));
        let added = previous.is_none();
        // insert already found the old slot. Only an absent local slot falls
        // through to the shared native table; a tombstone still hides it.
        let previous = previous
            .unwrap_or_else(|| {
                object
                    .shared_members
                    .as_ref()
                    .and_then(|table| table.get(&name).copied())
            })
            .map(|entry| entry.value(id));
        let invalidate_snapshot = std::mem::take(&mut object.native_snapshot);
        if added {
            self.allocation_debt = self
                .allocation_debt
                .saturating_add(size_of::<(SymbolId, Option<Member>)>());
        }
        if invalidate_snapshot {
            self.native_snapshots.remove(&id);
        }
        Ok(previous)
    }

    pub fn remove_member(&mut self, id: ObjId, name: SymbolId) -> Result<Option<Value>, HeapError> {
        let previous = self.lookup_member(id, name)?;
        let object = self.object_mut_unbarriered(id)?;
        if object
            .shared_members
            .as_ref()
            .is_some_and(|table| table.contains_key(&name))
        {
            object.members.insert(name, None);
        } else {
            object.members.remove(&name);
        }
        if std::mem::take(&mut object.native_snapshot) {
            self.native_snapshots.remove(&id);
        }
        Ok(previous)
    }

    pub(crate) fn copy_script_members(
        &mut self,
        class: ObjId,
        instance: ObjId,
    ) -> Result<(), HeapError> {
        let members: Vec<_> = self
            .object(class)?
            .entries()
            .filter(|(_, entry)| !entry.class_only)
            .collect();
        for (key, entry) in members {
            let mut value = entry.value;
            if let Value::Obj(reference) = &mut value {
                reference.this = Some(instance);
            }
            self.set_member_flags(instance, key, value, entry.hidden, false)?;
        }
        Ok(())
    }

    pub(super) fn copy_native_members(
        &mut self,
        class: ObjId,
        instance: ObjId,
    ) -> Result<(), HeapError> {
        let table = if let Some(table) = self.native_snapshots.get(&class) {
            Rc::clone(table)
        } else {
            let table: Table = self
                .object(class)?
                .entries()
                .filter(|(_, entry)| !entry.class_only)
                .map(|(key, mut entry)| {
                    entry.bind = true;
                    (key, entry)
                })
                .collect();
            self.allocation_debt = self
                .allocation_debt
                .saturating_add(table.len() * size_of::<(SymbolId, Member)>());
            let table = Rc::new(table);
            self.native_snapshots.insert(class, Rc::clone(&table));
            self.object_mut_unbarriered(class)?.native_snapshot = true;
            table
        };
        let object = self.object_mut(instance)?;
        if object.shared_members.is_none() && object.members.is_empty() {
            object.shared_members = Some(table);
        } else {
            for (&key, &entry) in table.iter() {
                self.set_member_flags(instance, key, entry.value(instance), entry.hidden, false)?;
            }
        }
        Ok(())
    }
}
