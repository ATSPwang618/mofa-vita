use super::{CallError, Vm};
use crate::{FunctionKind, Heap, Value};

impl Vm {
    pub(super) fn add_class_info_value(
        &self,
        heap: &mut Heap,
        object: Value,
        name: Value,
    ) -> Result<(), CallError> {
        let Value::Obj(reference) = object else {
            return Err(crate::member::MemberError::NotObject.into());
        };
        if let Some(id) = reference.object {
            let Value::Str(name) = name else {
                return Err(crate::value::ArithmeticError::UnsupportedOperands.into());
            };
            let name = heap.intern_string(name)?;
            heap.add_class_name(id, name)?;
        }
        Ok(())
    }
    pub(super) fn type_name(&mut self, heap: &mut Heap, value: Option<Value>) -> Value {
        let (index, name) = match value {
            Some(Value::Void) => (0, "void"),
            Some(Value::Obj(_)) => (1, "Object"),
            Some(Value::Str(_)) => (2, "String"),
            Some(Value::Int(_)) => (3, "Integer"),
            Some(Value::Real(_)) => (4, "Real"),
            Some(Value::Octet(_)) => (5, "Octet"),
            None => (6, "undefined"),
        };
        if matches!(self.type_names[index], Value::Void) {
            self.type_names[index] =
                Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>()));
        }
        self.type_names[index]
    }

    pub(super) fn add_class_info(&mut self, heap: &mut Heap) -> Result<(), CallError> {
        let FunctionKind::Class { constructor, .. } =
            *self.modules[self.frame.module].module.functions()[self.frame.function].kind()
        else {
            unreachable!("validated class instruction")
        };
        let Value::Str(name) = self.load_constant(heap, self.frame.module, constructor) else {
            unreachable!("class name")
        };
        let name = heap.intern_string(name)?;
        let instance = self.this(heap);
        heap.add_class_name(instance, name)?;
        Ok(())
    }
}
