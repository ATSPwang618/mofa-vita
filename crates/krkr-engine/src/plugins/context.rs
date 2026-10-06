use tjs_core::{Heap, NativeError, NativeResult, ObjId, SymbolId, Value};

pub(super) type MemberValue = (Value, bool, bool);
pub(super) type Pending = Vec<(ObjId, SymbolId, Option<MemberValue>)>;

/// A registration leaf's pending global exports. Changes become visible only
/// after link succeeds, or after unlink succeeds and accepts unloading.
/// Direct Heap mutations and provider-owned resources are not transactional;
/// prepare those with Rust ownership and finish fallible work before releasing
/// live resources. Registered native code stays valid after an export is removed.
pub struct Context<'a> {
    pub heap: &'a mut Heap,
    pub global: ObjId,
    pub(super) exports: Pending,
}
impl<'a> Context<'a> {
    pub(super) fn new(heap: &'a mut Heap, global: ObjId) -> Self {
        Self {
            heap,
            global,
            exports: Vec::new(),
        }
    }

    pub fn export(&mut self, name: &str, value: Value) -> NativeResult<()> {
        self.stage(self.global, name, Some(value))
    }

    pub fn remove(&mut self, name: &str) -> NativeResult<()> {
        self.stage(self.global, name, None)
    }

    /// Stage a class extension in the same transaction as global exports.
    pub fn export_member(&mut self, owner: ObjId, name: &str, value: Value) -> NativeResult<()> {
        self.stage(owner, name, Some(value))
    }

    /// Preserve native member visibility and static placement in the export transaction.
    pub fn export_member_with_flags(
        &mut self,
        owner: ObjId,
        name: &str,
        value: Value,
        hidden: bool,
        class_only: bool,
    ) -> NativeResult<()> {
        let key = self.heap.intern_str(name);
        self.heap.object(owner)?;
        self.exports
            .push((owner, key, Some((value, hidden, class_only))));
        Ok(())
    }

    pub fn remove_member(&mut self, owner: ObjId, name: &str) -> NativeResult<()> {
        self.stage(owner, name, None)
    }

    /// Raw lookup including this hook's pending edits; does not invoke getters.
    pub fn exported(&mut self, name: &str) -> NativeResult<Option<Value>> {
        let key = self.heap.intern_str(name);
        Ok(self.member(self.global, key)?.map(|v| v.0))
    }

    pub(super) fn member(&self, owner: ObjId, key: SymbolId) -> NativeResult<Option<MemberValue>> {
        if let Some((_, _, value)) = self
            .exports
            .iter()
            .rev()
            .find(|(target, name, _)| *target == owner && *name == key)
        {
            return Ok(*value);
        }
        Ok(self.heap.member_with_flags(owner, key)?)
    }

    pub(super) fn stage_key(
        &mut self,
        owner: ObjId,
        key: SymbolId,
        value: Option<MemberValue>,
    ) -> NativeResult<()> {
        self.heap.object(owner)?;
        self.exports.push((owner, key, value));
        Ok(())
    }

    fn stage(&mut self, owner: ObjId, name: &str, value: Option<Value>) -> NativeResult<()> {
        let key = self.heap.intern_str(name);
        self.heap.object(owner)?;
        self.exports
            .push((owner, key, value.map(|value| (value, false, false))));
        Ok(())
    }

    pub(super) fn validate(&self) -> NativeResult<()> {
        if !self.heap.is_valid(self.global)? {
            return Err(NativeError::Message("plugin global is invalid"));
        }
        // Validate the complete batch before writing. Hooks cannot collect or
        // reenter the VM, and these raw member operations execute no callbacks.
        for (owner, key, _) in &self.exports {
            self.heap.symbol(*key)?;
            if !self.heap.is_valid(*owner)? {
                return Err(NativeError::Message("plugin export target is invalid"));
            }
        }
        Ok(())
    }

    pub(super) fn commit(self) -> NativeResult<()> {
        self.validate()?;
        for (owner, key, value) in self.exports {
            match value {
                Some((value, hidden, class_only)) => {
                    self.heap
                        .set_member_flags(owner, key, value, hidden, class_only)?;
                }
                None => {
                    self.heap.remove_member(owner, key)?;
                }
            }
        }
        Ok(())
    }
}
