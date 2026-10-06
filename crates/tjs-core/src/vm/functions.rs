use super::{LoadedModule, Vm};
use crate::{Constant, FunctionId, FunctionKind, Heap, ObjId, ObjRef, Value, heap::ScriptFunction};

fn allocate(
    loaded: &mut LoadedModule,
    heap: &mut Heap,
    global: ObjId,
    pool: ObjId,
    function: FunctionId,
) -> Value {
    let object = heap.alloc_function(ScriptFunction {
        module: loaded.module.clone(),
        function,
        global,
        pool,
    });
    let value = Value::Obj(ObjRef {
        object: Some(object),
        this: None,
    });
    heap.cache_function(pool, function, value);
    value
}

impl Vm {
    pub(super) fn load_function(&mut self, heap: &mut Heap, function: FunctionId) -> Value {
        let global = self.ensure_global(heap);
        let loaded = &mut self.modules[self.frame.module];
        let constants = loaded.constants(heap);
        let pool = *loaded.function_pool.get_or_insert_with(|| {
            heap.alloc_function_pool(loaded.module.functions().len(), constants)
        });
        let cached = heap.function_value(pool, function);
        if !matches!(cached, Value::Void) {
            return cached;
        }
        let value = allocate(loaded, heap, global, pool, function);
        let definition = &loaded.module.functions()[function.0 as usize];
        if definition.members().is_empty()
            && matches!(
                definition.kind(),
                FunctionKind::Function | FunctionKind::Internal | FunctionKind::SuperResolver
            )
        {
            return value;
        }
        // Publish each cache entry before following member edges. The worklist
        // also supports cyclic metadata without recursive Rust calls.
        let mut pending = vec![function];
        while let Some(parent) = pending.pop() {
            let dependencies: Vec<_> = match loaded.module.functions()[parent.0 as usize].kind() {
                FunctionKind::Function | FunctionKind::Internal | FunctionKind::SuperResolver => {
                    Vec::new()
                }
                FunctionKind::Class { bases, .. } => bases.clone(),
                FunctionKind::Property { getter, setter } => {
                    getter.iter().chain(setter).copied().collect()
                }
            };
            for child in dependencies {
                if matches!(heap.function_value(pool, child), Value::Void) {
                    allocate(loaded, heap, global, pool, child);
                    pending.push(child);
                }
            }
            let Value::Obj(parent_object) = heap.function_value(pool, parent) else {
                unreachable!("cached function")
            };
            let count = loaded.module.functions()[parent.0 as usize].members().len();
            for index in 0..count {
                let member = loaded.module.functions()[parent.0 as usize].members()[index];
                let child = match heap.function_value(pool, member.function) {
                    Value::Void => {
                        let child = allocate(loaded, heap, global, pool, member.function);
                        pending.push(member.function);
                        child
                    }
                    value => value,
                };
                let Constant::String(name) = &loaded.module.constants()[member.name as usize]
                else {
                    unreachable!("validated member name")
                };
                let name = heap.intern(name);
                heap.set_member(parent_object.object.expect("function object"), name, child)
                    .expect("live function object");
            }
        }
        value
    }
}
