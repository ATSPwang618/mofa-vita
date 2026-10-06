mod full;

use super::{CollectionStats, Heap, HeapCounts, ObjId, ObjectData, SymbolId, members, strings};
use crate::Value;
use std::{collections::HashMap, rc::Rc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollectionPhase {
    #[default]
    Idle,
    Mark,
    Finalizers,
    FinalizerMark,
    Sweep,
}

/// One safe-point slice. Work counts object traces and arena entries examined.
/// Root enumeration, native Trace callbacks, one object's edges/destructor and
/// committing the finalizer set are indivisible; this is not a time deadline.
#[derive(Clone, Copy, Debug)]
pub struct CollectionStep {
    pub phase: CollectionPhase,
    pub work: usize,
    pub completed: Option<CollectionStats>,
}

#[derive(Default)]
pub(super) struct Collector {
    pub color: bool,
    pub phase: CollectionPhase,
    before: HeapCounts,
    remaining: [usize; 5],
    finalizer_cursor: usize,
    remark_pending: bool,
    // WeakObjId::upgrade accepts &Heap; defer its marking work to the next slice.
    weak_gray: std::cell::RefCell<Vec<ObjId>>,
    sweep_arena: usize,
    candidates: Vec<ObjId>,
    // Own each immutable snapshot until the end of the cycle: a dropped table's
    // address must not be reused for an untraced table between slices.
    tables: HashMap<*const members::Table, Rc<members::Table>>,
    accounted_tables: std::collections::HashSet<*const members::Table>,
    traced_objects: usize,
    retained_bytes: usize,
}

impl Heap {
    pub fn collection_phase(&self) -> CollectionPhase {
        self.gc.phase
    }

    pub fn is_collecting(&self) -> bool {
        self.gc.phase != CollectionPhase::Idle
    }

    /// Start or advance collection only between execution slices/native calls.
    /// Supply ALL current VM/host roots on EVERY call, including during sweep.
    /// Unrooted copied IDs cannot be used to recover a graph being swept; use
    /// WeakObjId::upgrade for weak references. New allocations survive this cycle.
    /// A zero budget does nothing. Allocating never initiates collection.
    pub fn collect_step(
        &mut self,
        roots: impl IntoIterator<Item = Value>,
        budget: usize,
    ) -> CollectionStep {
        let mut result = CollectionStep {
            phase: self.gc.phase,
            work: 0,
            completed: None,
        };
        if budget == 0 {
            return result;
        }
        if !self.is_collecting() {
            self.begin_collection();
        }
        self.gray.append(self.gc.weak_gray.get_mut());
        self.mark_roots(roots);
        let mut gray = std::mem::take(&mut self.gray);
        let mut tables = std::mem::take(&mut self.gc.tables);
        let mut native_remarked = !self.gc.remark_pending;
        while result.work < budget {
            // Mutations/root changes can produce gray work in ANY phase. Never
            // reclaim another slot until all of that work has been traced.
            if let Some(id) = gray.pop() {
                self.objects[id.0].data.gray.set(false);
                self.trace_object(id, &mut gray, &mut tables);
                self.gc.traced_objects += 1;
                result.work += 1;
                continue;
            }
            match self.gc.phase {
                CollectionPhase::Idle => unreachable!(),
                CollectionPhase::Mark => self.gc.phase = CollectionPhase::Finalizers,
                CollectionPhase::Finalizers if self.gc.finalizer_cursor != 0 => {
                    let count = self.gc.finalizer_cursor.min(budget - result.work);
                    for index in (self.gc.finalizer_cursor - count..self.gc.finalizer_cursor).rev()
                    {
                        let id = ObjId(self.objects.key_at(index));
                        if self.finalizer_candidate(id) {
                            self.gc.candidates.push(id);
                        }
                    }
                    self.gc.finalizer_cursor -= count;
                    result.work += count;
                }
                CollectionPhase::Finalizers if !native_remarked => {
                    self.remark_native(&mut gray);
                    native_remarked = true;
                }
                CollectionPhase::Finalizers => {
                    // Select the entire unreachable set BEFORE marking any of
                    // it, so every finalizable member of a cycle is queued.
                    // Recheck candidates: a mutator may have rooted/invalidated
                    // them since discovery. No script runs in this commit.
                    let mut candidates = std::mem::take(&mut self.gc.candidates);
                    candidates.retain(|&id| self.finalizer_candidate(id));
                    for id in candidates.drain(..) {
                        self.objects[id.0].data.gc_finalized = true;
                        self.pending_finalizers.push_back(id);
                        self.mark_value(Value::Obj(id.into()), &mut gray);
                    }
                    self.gc.candidates = candidates;
                    self.gc.phase = CollectionPhase::FinalizerMark;
                }
                CollectionPhase::FinalizerMark if !native_remarked => {
                    self.remark_native(&mut gray);
                    native_remarked = true;
                }
                CollectionPhase::FinalizerMark => self.gc.phase = CollectionPhase::Sweep,
                CollectionPhase::Sweep => {
                    while self.gc.sweep_arena < 5 && self.gc.remaining[self.gc.sweep_arena] == 0 {
                        self.gc.sweep_arena += 1;
                    }
                    if self.gc.sweep_arena == 5 {
                        result.completed = Some(CollectionStats {
                            before: self.gc.before,
                            after: self.counts(),
                            traced_objects: self.gc.traced_objects,
                            retained_bytes: self.gc.retained_bytes,
                        });
                        self.gc.phase = CollectionPhase::Idle;
                        tables.clear();
                        break;
                    }
                    let arena = self.gc.sweep_arena;
                    let end = self.gc.remaining[arena];
                    let count = end.min(budget - result.work);
                    let range = end - count..end;
                    match arena {
                        0 => self.sweep_entries::<0>(range),
                        1 => self.sweep_entries::<1>(range),
                        2 => self.sweep_entries::<2>(range),
                        3 => self.sweep_entries::<3>(range),
                        4 => self.sweep_entries::<4>(range),
                        _ => unreachable!(),
                    }
                    self.gc.remaining[arena] -= count;
                    result.work += count;
                }
            }
        }
        self.gray = gray;
        self.gc.tables = tables;
        self.gc.remark_pending = self.is_collecting();
        result.phase = self.gc.phase;
        result
    }

    fn begin_collection(&mut self) {
        for id in self
            .gray
            .drain(..)
            .chain(self.gc.weak_gray.get_mut().drain(..))
        {
            self.objects[id.0].data.gray.set(false);
        }
        self.gc.tables.clear();
        self.gc.candidates.clear();
        self.gc.accounted_tables.clear();
        // Ordinary cycles flip one bit without clearing the heap. Restarting
        // a partial cycle needs normalization because it contains both colors;
        // only explicit full collection takes that synchronous path.
        if self.is_collecting() {
            for entry in self.objects.values() {
                entry.marked.set(self.gc.color);
            }
            for entry in self.strings.values() {
                entry.marked.set(self.gc.color);
            }
            for entry in self.string_buffers.values() {
                entry.marked.set(self.gc.color);
            }
            for entry in self.octets.values() {
                entry.marked.set(self.gc.color);
            }
            for entry in self.symbols.values() {
                entry.marked.set(self.gc.color);
            }
        }
        self.gc.color = !self.gc.color;
        self.gc.phase = CollectionPhase::Mark;
        self.gc.before = self.counts();
        self.gc.remaining = [
            self.objects.len(),
            self.strings.len(),
            self.string_buffers.len(),
            self.octets.len(),
            self.symbols.len(),
        ];
        self.gc.finalizer_cursor = self.objects.len();
        self.gc.remark_pending = false;
        self.gc.sweep_arena = 0;
        self.gc.traced_objects = 0;
        self.gc.retained_bytes = 0;
        // Keep debt accrued DURING the cycle for the next collection trigger.
        self.allocation_debt = 0;
    }

    pub(super) fn write_barrier(&mut self, id: ObjId) {
        if self.is_collecting()
            && let Some(entry) = self.objects.get(id.0)
            && entry.marked.get() == self.gc.color
            && !entry.data.gray.replace(true)
        {
            self.gray.push(id);
        }
    }

    fn marked_object(&self, id: ObjId) -> bool {
        self.objects
            .get(id.0)
            .is_some_and(|entry| entry.marked.get() == self.gc.color)
    }

    /// Known container writes shade the inserted edges, not the whole owner.
    /// In particular, numeric updates to a large array never rescan that array.
    pub(super) fn edge_barrier(&mut self, owner: ObjId, values: impl IntoIterator<Item = Value>) {
        if self.is_collecting() && self.marked_object(owner) {
            let mut gray = std::mem::take(&mut self.gray);
            for value in values {
                self.mark_value(value, &mut gray);
            }
            self.gray = gray;
        }
    }

    pub(super) fn symbol_barrier(&self, owner: ObjId, name: SymbolId) {
        if self.is_collecting() && self.marked_object(owner) {
            self.mark_symbol(name);
        }
    }

    fn mark_roots(&mut self, roots: impl IntoIterator<Item = Value>) {
        let mut gray = std::mem::take(&mut self.gray);
        for value in self
            .roots
            .values()
            .copied()
            .chain(roots)
            .chain(
                self.native_classes
                    .values()
                    .map(|&id| Value::Obj(id.into())),
            )
            .chain(
                self.pending_finalizers
                    .iter()
                    .map(|&id| Value::Obj(id.into())),
            )
        {
            self.mark_value(value, &mut gray);
        }
        if let Some(storage) = &self.storage {
            storage.trace(&mut |value| self.mark_value(value, &mut gray));
        }
        self.gray = gray;
    }

    fn remark_native(&self, gray: &mut Vec<ObjId>) {
        // Native states can hold externally mutable Rc<RefCell<_>> graphs.
        // Revisit black owners before committing finalizers / starting sweep.
        // If tracing spills into another slice, remark again after the mutator.
        // Once sweep starts every legally held value is already marked or was
        // allocated this cycle; weak upgrades cannot resurrect white graphs.
        for entry in self.objects.values() {
            if entry.marked.get() == self.gc.color && !entry.data.gray.get() {
                for state in entry.data.native.iter().flatten() {
                    state.trace(&mut |value| self.mark_value(value, gray));
                }
            }
        }
    }

    fn mark_value(&self, value: Value, gray: &mut Vec<ObjId>) {
        match value {
            Value::Obj(reference) => {
                for id in [reference.object, reference.this].into_iter().flatten() {
                    if let Some(entry) = self.objects.get(id.0)
                        && entry.marked.replace(self.gc.color) != self.gc.color
                    {
                        entry.data.gray.set(true);
                        gray.push(id);
                    }
                }
            }
            Value::Str(id) => {
                if let Some(entry) = self.strings.get(id.0) {
                    entry.marked.set(self.gc.color);
                    if let strings::Data::Prefix { buffer, length } = entry.data {
                        self.mark_string_buffer(buffer, length);
                    }
                }
            }
            Value::Octet(id) => {
                if let Some(entry) = self.octets.get(id.0) {
                    entry.marked.set(self.gc.color);
                }
            }
            Value::Void | Value::Int(_) | Value::Real(_) => {}
        }
    }

    pub(super) fn mark_string_buffer(&self, key: super::StringBufferKey, length: usize) {
        let buffer = &self.string_buffers[key];
        let length = if buffer.marked.replace(self.gc.color) == self.gc.color {
            length.max(buffer.data.live_length.get())
        } else {
            length
        };
        buffer.data.live_length.set(length);
    }

    fn mark_symbol(&self, symbol: SymbolId) {
        if let Some(entry) = self.symbols.get(symbol.0) {
            entry.marked.set(self.gc.color);
        }
    }

    fn trace_object(
        &self,
        id: ObjId,
        gray: &mut Vec<ObjId>,
        tables: &mut HashMap<*const members::Table, Rc<members::Table>>,
    ) {
        let object = &self.objects[id.0].data;
        for (&symbol, &value) in &object.members {
            self.mark_symbol(symbol);
            if let Some(value) = value {
                self.mark_value(value.value, gray);
            }
        }
        if let Some(table) = &object.shared_members
            && let std::collections::hash_map::Entry::Vacant(entry) =
                tables.entry(Rc::as_ptr(table))
        {
            entry.insert(Rc::clone(table));
            for (&symbol, entry) in table.iter() {
                self.mark_symbol(symbol);
                self.mark_value(entry.value, gray);
            }
        }
        for &name in &object.class_names {
            self.mark_symbol(name);
        }
        if let ObjectData::Array(array) | ObjectData::FunctionPool(array) = &object.data {
            for &value in array {
                self.mark_value(value, gray);
            }
        }
        if let ObjectData::Scope { this, global } = object.data {
            self.mark_value(Value::Obj(this.into()), gray);
            self.mark_value(Value::Obj(global.into()), gray);
        }
        if let ObjectData::Function(function) = &object.data {
            self.mark_value(Value::Obj(function.global.into()), gray);
            self.mark_value(Value::Obj(function.pool.into()), gray);
        }
        if let ObjectData::NativeConstructor(class) = object.data {
            self.mark_value(Value::Obj(class.into()), gray);
        }
        if let ObjectData::NativeProperty { get, set, .. } = object.data {
            for id in get.into_iter().chain(set) {
                self.mark_value(Value::Obj(id.into()), gray);
            }
        }
        for state in object.native.iter().flatten() {
            state.trace(&mut |value| self.mark_value(value, gray));
        }
    }

    fn sweep_entries<const ARENA: usize>(&mut self, range: std::ops::Range<usize>) {
        for index in range.rev() {
            self.sweep_entry::<ARENA>(index);
        }
    }

    fn sweep_entry<const ARENA: usize>(&mut self, index: usize) {
        let color = self.gc.color;
        let retained = match ARENA {
            0 => {
                let key = self.objects.key_at(index);
                let entry = &self.objects[key];
                if entry.marked.get() != color {
                    self.objects.remove_at(index);
                    return;
                }
                let object = &entry.data;
                let mut bytes = size_of_val(entry)
                    + object.members.capacity() * size_of::<(SymbolId, Option<members::Member>)>()
                    + object.class_names.capacity() * size_of::<SymbolId>()
                    + object.native.capacity()
                        * size_of::<Option<Box<dyn crate::native::NativeState>>>();
                if let ObjectData::Array(values) | ObjectData::FunctionPool(values) = &object.data {
                    bytes += values.capacity() * size_of::<Value>();
                }
                for state in object.native.iter().flatten() {
                    bytes += size_of_val(state.any());
                }
                // Count immutable shared member storage once per cycle.
                if let Some(table) = &object.shared_members {
                    let pointer = Rc::as_ptr(table);
                    if self.gc.accounted_tables.insert(pointer) {
                        bytes += table.capacity() * size_of::<(SymbolId, members::Member)>();
                    }
                }
                bytes
            }
            1 => {
                let key = self.strings.key_at(index);
                let entry = &self.strings[key];
                if entry.marked.get() != color {
                    self.strings.remove_at(index);
                    return;
                }
                size_of_val(entry)
                    + match &entry.data {
                        strings::Data::Owned(units) => size_of_val(units.as_ref()),
                        strings::Data::Prefix { .. } => 0,
                    }
            }
            2 => {
                let key = self.string_buffers.key_at(index);
                let entry = &mut self.string_buffers[key];
                if entry.marked.get() != color {
                    self.string_buffers.remove_at(index);
                    return;
                }
                let bytes = size_of_val(entry);
                let buffer = &mut entry.data;
                let length = buffer.live_length.get();
                buffer.units.truncate(length);
                if buffer.units.capacity() / 4 > length.max(1024) {
                    buffer.units.shrink_to_fit();
                }
                bytes + buffer.units.capacity() * size_of::<u16>()
            }
            3 => {
                let key = self.octets.key_at(index);
                let entry = &self.octets[key];
                if entry.marked.get() != color {
                    self.octets.remove_at(index);
                    return;
                }
                size_of_val(entry) + entry.data.len()
            }
            4 => {
                let key = self.symbols.key_at(index);
                let entry = &self.symbols[key];
                if entry.marked.get() != color {
                    self.symbol_index.remove(entry.data.as_ref());
                    self.symbols.remove_at(index);
                    return;
                }
                size_of_val(entry) + size_of_val(entry.data.as_ref())
            }
            _ => unreachable!(),
        };
        self.gc.retained_bytes = self.gc.retained_bytes.saturating_add(retained);
    }

    pub(super) fn weak_object(&self, id: ObjId) -> Option<ObjId> {
        let entry = self.objects.get(id.0)?;
        // Sweep may already have reclaimed this white object's descendants.
        if self.gc.phase == CollectionPhase::Sweep && entry.marked.get() != self.gc.color {
            return None;
        }
        if self.is_collecting() && entry.marked.get() != self.gc.color {
            self.mark_value(Value::Obj(id.into()), &mut self.gc.weak_gray.borrow_mut());
        }
        Some(id)
    }
}

#[cfg(test)]
mod tests;
