//! The synchronous path keeps arena scans contiguous and skips slice dispatch.
use super::*;

impl Heap {
    /// Complete a fresh collection, discarding any incremental marking first.
    /// This also reclaims floating garbage retained by an interrupted cycle.
    /// Supply every active/suspended VM's roots and temporary host results.
    /// Finalizers remain queued for the VM; collection never executes scripts.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = Value>) -> CollectionStats {
        self.begin_collection();
        let before = self.counts();
        let color = self.gc.color;
        let mut tables = std::mem::take(&mut self.gc.tables);
        let mut gray = std::mem::take(&mut self.gray);
        for value in self.roots.values().copied().chain(roots).chain(
            self.native_classes
                .values()
                .map(|&id| Value::Obj(id.into())),
        ) {
            self.mark_value(value, &mut gray);
        }
        if let Some(storage) = &self.storage {
            storage.trace(&mut |value| self.mark_value(value, &mut gray));
        }
        for &id in &self.pending_finalizers {
            self.mark_value(Value::Obj(id.into()), &mut gray);
        }
        let mut traced_objects = 0;
        traced_objects += self.trace_all(&mut gray, &mut tables);
        self.queue_finalizers_now();
        for &id in &self.pending_finalizers {
            self.mark_value(Value::Obj(id.into()), &mut gray);
        }
        traced_objects += self.trace_all(&mut gray, &mut tables);
        self.gray = gray;
        let mut retained_bytes = 0usize;
        let mut shared_tables = std::collections::HashSet::new();
        self.objects.retain(|_, entry| {
            if entry.marked.get() != color {
                return false;
            }
            let object = &entry.data;
            retained_bytes += size_of_val(entry)
                + object.members.capacity()
                    * size_of::<(super::SymbolId, Option<super::members::Member>)>()
                + object.class_names.capacity() * size_of::<super::SymbolId>()
                + object.native.capacity()
                    * size_of::<Option<Box<dyn crate::native::NativeState>>>();
            if let Some(table) = &object.shared_members
                && shared_tables.insert(Rc::as_ptr(table))
            {
                retained_bytes +=
                    table.capacity() * size_of::<(super::SymbolId, super::members::Member)>();
            }
            if let ObjectData::Array(values) | ObjectData::FunctionPool(values) = &object.data {
                retained_bytes += values.capacity() * size_of::<Value>();
            }
            for state in object.native.iter().flatten() {
                retained_bytes += size_of_val(state.any());
            }
            true
        });
        self.strings.retain(|_, entry| {
            let live = entry.marked.get() == color;
            if live {
                retained_bytes += size_of_val(entry);
                match &entry.data {
                    super::strings::Data::Owned(units) => {
                        retained_bytes += size_of_val(units.as_ref());
                    }
                    super::strings::Data::Prefix { .. } => {}
                }
            }
            live
        });
        self.string_buffers.retain(|_, entry| {
            if entry.marked.get() != color {
                return false;
            }
            let buffer = &mut entry.data;
            // A surviving short prefix must not retain an abandoned large tail.
            // No external slice borrow is active at this collection boundary.
            buffer.units.truncate(buffer.live_length.get());
            if buffer.units.capacity() / 4 > buffer.live_length.get().max(1024) {
                buffer.units.shrink_to_fit();
            }
            retained_bytes += size_of_val(entry) + entry.data.units.capacity() * size_of::<u16>();
            true
        });
        self.octets.retain(|_, entry| {
            let live = entry.marked.get() == color;
            if live {
                retained_bytes += size_of_val(entry) + entry.data.len();
            }
            live
        });
        self.symbols.retain(|_, entry| {
            let live = entry.marked.get() == color;
            if !live {
                self.symbol_index.remove(entry.data.as_ref());
            } else {
                retained_bytes += size_of_val(entry) + size_of_val(entry.data.as_ref());
            }
            live
        });
        self.gc.phase = CollectionPhase::Idle;
        tables.clear();
        self.gc.tables = tables;
        CollectionStats {
            before,
            after: self.counts(),
            traced_objects,
            retained_bytes,
        }
    }

    fn trace_all(
        &self,
        gray: &mut Vec<ObjId>,
        tables: &mut HashMap<*const members::Table, Rc<members::Table>>,
    ) -> usize {
        let mut count = 0;
        while let Some(id) = gray.pop() {
            self.objects[id.0].data.gray.set(false);
            self.trace_object(id, gray, tables);
            count += 1;
        }
        count
    }

    fn queue_finalizers_now(&mut self) {
        for index in 0..self.objects.len() {
            let id = ObjId(self.objects.key_at(index));
            if self.finalizer_candidate(id) {
                self.gc.candidates.push(id);
            }
        }
        for id in self.gc.candidates.drain(..) {
            self.objects[id.0].data.gc_finalized = true;
            self.pending_finalizers.push_back(id);
        }
    }
}
