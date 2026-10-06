//! TJS primitive methods enter the same dispatch path as ordinary member calls.
use super::{
    CallError, Vm,
    calls::{Invocation, ReturnTo},
    dispatch::{Access, Action},
};
use crate::{Heap, NativeError, ObjRef, StrId, Value, member::MemberError, value};

impl Vm {
    pub(super) fn call_string(
        &mut self,
        heap: &mut Heap,
        string: StrId,
        key: Value,
        mut call: Invocation,
    ) -> Result<(), CallError> {
        let Value::Str(name) = value::to_string(heap, key)? else {
            unreachable!()
        };
        let name = heap.string(name)?;
        let name = &name[..name
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(name.len())];
        let replace = name.iter().copied().eq("replace".encode_utf16());
        let split = name.iter().copied().eq("split".encode_utf16());
        if !replace && !split {
            let method = crate::string::Method::from_name(name).ok_or(MemberError::Missing)?;
            let result = crate::string::call(
                heap,
                string,
                method,
                &self.registers[call.start..call.end],
                call.destination.result_needed(),
            )?;
            let context = call.this.unwrap_or_else(|| self.this(heap));
            return self.native_step(heap, context, call, result, true);
        }
        let count = call.end - call.start;
        if count < if replace { 2 } else { 1 } {
            return Err(NativeError::Missing(count + 1).into());
        }
        let pattern = self.registers[call.start];
        if replace {
            // ProcessStringFunction delegates to the pattern object's replace
            // method, with the original result demand. Strings are not patterns.
            if !matches!(pattern, Value::Obj(_)) {
                return Err(NativeError::Type("an object replacement pattern").into());
            }
            call.end = call.start + 2;
            self.registers.truncate(call.end);
            self.registers[call.start] = Value::Str(string);
            return self.access(
                heap,
                pattern,
                key,
                Access::Call {
                    invocation: call,
                    instance: None,
                },
                None,
                true,
            );
        }

        // String.split is the Array.split shorthand, including character-set
        // delimiters, reserved arguments and RegExp's no-captured-delimiters rule.
        if call.end >= self.limits.max_stack_values {
            return Err(CallError::Stack);
        }
        call.end = call.start + count.min(3);
        self.registers.truncate(call.end);
        self.registers.insert(call.start + 1, Value::Str(string));
        call.end += 1;
        let array = heap.alloc_array();
        self.push_action(Action::Instance {
            instance: array,
            invocation: call,
        });
        call.destination = ReturnTo::ResumeDiscard;
        self.access(
            heap,
            Value::Obj(ObjRef::bound(array)),
            key,
            Access::Call {
                invocation: call,
                instance: None,
            },
            None,
            true,
        )
    }
}
