use super::{Heap, HeapError, ObjId, ObjectData, ObjectKind, SymbolId};

impl Heap {
    pub fn class_names(&self, id: ObjId) -> Result<Vec<Vec<u16>>, HeapError> {
        let object = self.object(id)?;
        object.ensure_valid()?;
        object
            .class_names
            .iter()
            .map(|&name| self.symbol(name).map(|units| units.to_vec()))
            .collect()
    }

    pub(crate) fn add_class_name(&mut self, id: ObjId, name: SymbolId) -> Result<(), HeapError> {
        self.symbol_barrier(id, name);
        let names = &mut self.object_mut_unbarriered(id)?.class_names;
        let capacity = names.capacity();
        if names.contains(&name) {
            return Ok(());
        }
        names.push(name);
        let added = (names.capacity() - capacity) * size_of::<SymbolId>();
        self.allocation_debt = self.allocation_debt.saturating_add(added);
        Ok(())
    }

    pub(crate) fn instance_of(&self, id: ObjId, name: &[u16]) -> Result<bool, HeapError> {
        let object = self.object(id)?;
        object.ensure_valid()?;
        let equal = |text: &str| text.encode_utf16().eq(name.iter().copied());
        let kind_name = match object.kind() {
            ObjectKind::Function if matches!(&object.data, ObjectData::Function(function) if matches!(function.kind(), crate::FunctionKind::Internal | crate::FunctionKind::SuperResolver)) => {
                ""
            }
            ObjectKind::Function | ObjectKind::NativeFunction => "Function",
            ObjectKind::Property | ObjectKind::NativeProperty => "Property",
            ObjectKind::Class | ObjectKind::NativeClass => "Class",
            ObjectKind::Array => "Array",
            ObjectKind::Dictionary => "Dictionary",
            _ => "",
        };
        if !kind_name.is_empty() && equal(kind_name) {
            return Ok(true);
        }
        if let ObjectData::NativeClass(class) = &object.data {
            if equal(class.name) {
                return Ok(true);
            }
        }
        Ok(self
            .find_symbol(name)
            .is_some_and(|name| object.class_names.contains(&name)))
    }
}
