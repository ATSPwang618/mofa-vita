use super::{Heap, ObjId, ObjectData};
use crate::{Constant, ConstantValue, FunctionId, Module, ObjRef, Value};

impl Heap {
    pub(crate) fn alloc_scope(&mut self, this: ObjId, global: ObjId) -> ObjId {
        self.has_scopes = true;
        self.alloc_record(ObjectData::Scope { this, global })
    }

    pub(crate) fn scope(&self, value: Value) -> Result<Option<(ObjId, ObjId)>, super::HeapError> {
        if !self.has_scopes {
            return Ok(None);
        }
        let Value::Obj(reference) = value else {
            return Ok(None);
        };
        let Some(id) = reference.object else {
            return Ok(None);
        };
        Ok(match self.object(id)?.data {
            ObjectData::Scope { this, global } => Some((this, global)),
            _ => None,
        })
    }

    pub(crate) fn alloc_function_pool(&mut self, count: usize, constants: ObjId) -> ObjId {
        let mut values = vec![Value::Void; count];
        values.push(Value::Obj(constants.into()));
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(values.len() * size_of::<Value>());
        self.alloc_record(ObjectData::FunctionPool(values))
    }

    pub(crate) fn function_constants(&self, pool: ObjId) -> ObjId {
        let ObjectData::FunctionPool(values) = &self.object(pool).expect("live function pool").data
        else {
            unreachable!("internal function pool")
        };
        let Some(Value::Obj(constants)) = values.last() else {
            unreachable!("constant pool edge")
        };
        constants.object.expect("constant pool")
    }

    pub(crate) fn alloc_constant_pool(&mut self, module: &Module) -> ObjId {
        let mut values = Vec::with_capacity(module.constants().len());
        // Functions retain this traced pool across VM instances. A VM reset
        // can replace its global/function context while retaining literal data.
        for constant in module.constants() {
            let resolve = |value: ConstantValue| match value {
                ConstantValue::Void => Value::Void,
                ConstantValue::Int(value) => Value::Int(value),
                ConstantValue::Real(value) => Value::Real(value),
                ConstantValue::Null => Value::Obj(ObjRef::default()),
                ConstantValue::Reference(index) => values[index as usize],
            };
            let value = match constant {
                Constant::String(units) => Value::Str(self.alloc_string(units.as_ref())),
                Constant::Octet(bytes) => Value::Octet(self.alloc_octet(bytes.clone())),
                Constant::Array(elements) => {
                    let array = self.alloc_array();
                    for &element in elements {
                        self.array_push(array, resolve(element)).expect("new array");
                    }
                    Value::Obj(ObjRef::bound(array))
                }
                Constant::Dictionary(entries) => {
                    let dictionary = self.alloc_dictionary();
                    for (name, value) in entries {
                        let name = self.intern(name);
                        self.set_member(dictionary, name, resolve(*value))
                            .expect("new dictionary");
                    }
                    Value::Obj(ObjRef::bound(dictionary))
                }
            };
            values.push(value);
        }
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(values.len() * size_of::<Value>());
        self.alloc_record(ObjectData::FunctionPool(values))
    }

    pub(crate) fn constant_value(&self, pool: ObjId, constant: u32) -> Value {
        let ObjectData::FunctionPool(values) = &self.object(pool).expect("live function pool").data
        else {
            unreachable!("internal function pool")
        };
        values[constant as usize]
    }

    pub(crate) fn function_value(&self, pool: ObjId, function: FunctionId) -> Value {
        let ObjectData::FunctionPool(values) = &self.object(pool).expect("live function pool").data
        else {
            unreachable!("internal function pool")
        };
        values[function.0 as usize]
    }

    pub(crate) fn cache_function(&mut self, pool: ObjId, function: FunctionId, value: Value) {
        self.edge_barrier(pool, [value]);
        let ObjectData::FunctionPool(values) = &mut self
            .object_mut_unbarriered(pool)
            .expect("live function pool")
            .data
        else {
            unreachable!("internal function pool")
        };
        values[function.0 as usize] = value;
    }
}
