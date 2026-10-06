//! Object invalidation is separate from reclaiming its slot.
use super::{Heap, HeapError, ObjId, ObjectData, ObjectKind};
use crate::Value;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Life {
    Live,
    Invalid,
}

// The VM owns this token. Weak ownership in the record makes reset/drop/unwind
// cancel finalization without a long-lived heap borrow or a destructor callback.
pub(crate) struct Finalization {
    pub object: ObjId,
    pub next_native: usize,
    _active: Rc<()>,
}

pub(crate) enum BeginFinalization {
    Unsupported,
    AlreadyInvalid,
    Reentrant,
    Started(Finalization),
}

impl Heap {
    /// Script validity, distinct from slot liveness. An object remains valid
    /// while its finalizer runs and when that callback throws or is cancelled.
    pub fn is_valid(&self, id: ObjId) -> Result<bool, HeapError> {
        Ok(self.object(id)?.life != Life::Invalid)
    }

    pub fn ensure_valid(&self, id: ObjId) -> Result<(), HeapError> {
        self.object(id)?.ensure_valid()
    }

    /// Whether a VM still owns this object's finalization. Hosts can defer
    /// shutdown through native cleanup without retaining or prolonging it.
    pub fn is_finalizing(&self, id: ObjId) -> Result<bool, HeapError> {
        Ok(self.object(id)?.finalizing.strong_count() != 0)
    }

    pub(crate) fn begin_finalization(&mut self, id: ObjId) -> Result<BeginFinalization, HeapError> {
        let object = self.object_mut(id)?;
        if matches!(
            object.kind(),
            ObjectKind::NativeFunction | ObjectKind::NativeProperty | ObjectKind::FunctionPool
        ) {
            return Ok(BeginFinalization::Unsupported);
        }
        if object.life == Life::Invalid {
            return Ok(BeginFinalization::AlreadyInvalid);
        }
        if object.finalizing.strong_count() != 0 {
            return Ok(BeginFinalization::Reentrant);
        }
        let active = Rc::new(());
        object.finalizing = Rc::downgrade(&active);
        Ok(BeginFinalization::Started(Finalization {
            object: id,
            next_native: object.native.len(),
            _active: active,
        }))
    }

    pub(crate) fn next_native_invalidator(
        &self,
        guard: &mut Finalization,
    ) -> Result<Option<crate::NativeCallable>, HeapError> {
        let object = self.object(guard.object)?;
        while guard.next_native > 0 {
            guard.next_native -= 1;
            if let Some(state) = &object.native[guard.next_native]
                && let Some(&call) = self.native_invalidators.get(&state.any().type_id())
            {
                return Ok(Some(call));
            }
        }
        Ok(None)
    }

    fn has_native_invalidator(&self, object: &super::ObjRecord) -> bool {
        object.native.iter().flatten().any(|state| {
            self.native_invalidators
                .contains_key(&state.any().type_id())
        })
    }

    pub(crate) fn finish_finalization(&mut self, guard: Finalization) -> Result<(), HeapError> {
        let object = self.object_mut(guard.object)?;
        // Script finalize has already returned. Release native resources and
        // member edges only now; saved references keep an invalid object.
        if let ObjectData::Array(values) = &mut object.data {
            values.clear();
        }
        object.native.clear();
        object.shared_members = None;
        object.members.clear();
        object.life = Life::Invalid;
        object.finalizing = std::rc::Weak::new();
        if std::mem::take(&mut object.native_snapshot) {
            self.native_snapshots.remove(&guard.object);
        }
        Ok(())
    }

    pub(crate) fn calls_finalize(&self, id: ObjId) -> Result<bool, HeapError> {
        // The reference disables script finalizers on arrays, dictionaries,
        // code contexts and native class descriptions.
        Ok(self.object(id)?.kind() == ObjectKind::Plain)
    }

    /// Unclaimed GC finalizers retained as heap roots. Claim them with
    /// `Vm::take_finalizer`; collection itself never invokes scripts.
    pub fn pending_finalizers(&self) -> usize {
        self.pending_finalizers.len()
    }

    pub(crate) fn take_finalizer(&mut self) -> Option<ObjId> {
        self.pending_finalizers.pop_front()
    }

    pub(super) fn finalizer_candidate(&self, id: ObjId) -> bool {
        let entry = &self.objects[id.0];
        let object = &entry.data;
        if entry.marked.get() == self.gc.color
            || object.gc_finalized
            || object.life != Life::Live
            || object.kind() != ObjectKind::Plain
            || object.finalizing.strong_count() != 0
        {
            return false;
        }
        if self.has_native_invalidator(object) {
            return true;
        }
        let Some(name) = self.find_symbol(&[102, 105, 110, 97, 108, 105, 122, 101]) else {
            return false;
        };
        let Ok(Some(Value::Obj(reference))) = self.lookup_member(id, name) else {
            return false;
        };
        let Some(function) = reference.object else {
            return false;
        };
        !matches!(
            self.native_callable(function, false),
            Ok(Some(crate::NativeCallable::EmptyFinalizer(_)))
        )
    }
}
