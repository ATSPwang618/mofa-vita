use super::*;

// Inheritance adds native container storage without changing the receiving
// object's dispatch type (an ordinary object does not gain numeric indexing).
#[derive(Default)]
pub(super) struct InheritedArray(pub Vec<Value>);
impl Trace for InheritedArray {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        Trace::trace(&self.0, visit);
    }
}
#[derive(Default)]
pub(super) struct InheritedDictionary;
impl Trace for InheritedDictionary {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}

impl Heap {
    /// Native container identity for assign/save operations. Script member
    /// dispatch still uses ObjectKind, preserving the legacy inheritance rules.
    pub fn container_kind(&self, id: ObjId) -> Result<ObjectKind, HeapError> {
        let record = self.object(id)?;
        record.ensure_valid()?;
        if matches!(record.data, ObjectData::Array(_))
            || record
                .native
                .iter()
                .flatten()
                .any(|state| state.any().is::<InheritedArray>())
        {
            return Ok(ObjectKind::Array);
        }
        if matches!(record.data, ObjectData::Dictionary)
            || record
                .native
                .iter()
                .flatten()
                .any(|state| state.any().is::<InheritedDictionary>())
        {
            return Ok(ObjectKind::Dictionary);
        }
        Ok(record.kind())
    }

    pub fn array(&self, id: ObjId) -> Result<&[Value], HeapError> {
        let record = self.object(id)?;
        record.ensure_valid()?;
        if let ObjectData::Array(array) = &record.data {
            return Ok(array);
        }
        record
            .native
            .iter()
            .flatten()
            .find_map(|state| {
                state
                    .any()
                    .downcast_ref::<InheritedArray>()
                    .map(|array| array.0.as_slice())
            })
            .ok_or(HeapError::NotArray)
    }

    pub(super) fn array_mut(&mut self, id: ObjId) -> Result<&mut Vec<Value>, HeapError> {
        let record = self.object_mut_unbarriered(id)?;
        record.ensure_valid()?;
        if let ObjectData::Array(array) = &mut record.data {
            return Ok(array);
        }
        record
            .native
            .iter_mut()
            .flatten()
            .find_map(|state| {
                state
                    .any_mut()
                    .downcast_mut::<InheritedArray>()
                    .map(|array| &mut array.0)
            })
            .ok_or(HeapError::NotArray)
    }
}
