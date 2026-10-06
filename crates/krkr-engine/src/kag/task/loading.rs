use super::*;
struct Pending {
    task: Box<Task>,
    name: Text,
    operations: Option<crate::operations::Shared>,
}
impl Trace for Pending {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.task.trace(visit);
    }
}
pub(super) fn begin(
    cx: &mut NativeCx<'_>,
    task: Box<Task>,
    name: Text,
    operations: Option<crate::operations::Shared>,
) -> NativeResult<NativeStep> {
    crate::storages::managed::plans(
        cx,
        vec![(name.clone(), true)],
        Pending {
            task,
            name,
            operations,
        },
        |mut pending, cx, mut plans| {
            let plan = plans.pop().flatten().expect("required scenario plan");
            let limit = crate::storages::service(cx)?
                .borrow()
                .limits()
                .max_read_bytes;
            let delivery = crate::io::Delivery::default();
            pending.task.commands.push(Command::ReadValue {
                name: pending.name,
                delivery: delivery.clone(),
            });
            if let Some(operations) = pending.operations {
                let read = crate::io::Read {
                    plan,
                    offset: 0,
                    encoding: units("utf-8"),
                    limit,
                };
                crate::operations::Operations::wait(
                    &operations,
                    crate::operations::Request::Read(Box::new(read.into()), delivery),
                    WaitMode::Internal,
                    pending.task,
                )
            } else {
                let bytes = plan
                    .read(0)
                    .map_err(|e| NativeError::Detail(e.to_string()))?;
                let source = krkr_assets::text::decode(&bytes, &units("utf-8"), limit)
                    .map_err(|e| NativeError::Detail(e.to_string()))?;
                *delivery.borrow_mut() = Some(crate::io::Data::Text(source));
                pending.task.resume(cx, Value::Void)
            }
        },
    )
}
impl Task {
    pub(super) fn load_value(
        &mut self,
        cx: &mut NativeCx<'_>,
        name: Text,
        value: Value,
    ) -> NativeResult<Flow> {
        if let Value::Str(id) = value {
            let source = cx.heap().string(id)?.to_vec();
            let scenario = Arc::new(Scenario::new(source).map_err(error)?);
            cx.with_state::<State, _>(|s, _| {
                s.parser.load(name.clone(), scenario);
                Ok(())
            })?;
            self.commands.push(Command::Loaded(name));
            return Ok(Flow::Next);
        }
        let cached = cx.with_state::<State, _>(|s, _| Ok(s.cache.borrow_mut().get(&name)))?;
        if let Some(scenario) = cached {
            cx.with_state::<State, _>(|s, _| {
                s.parser.load(name.clone(), scenario);
                Ok(())
            })?;
            self.commands.push(Command::Loaded(name));
            return Ok(Flow::Next);
        }
        let operations = cx.with_state::<State, _>(|s, _| Ok(s.operations.clone()))?;
        if cx.heap().registered_class("Storages").is_some() {
            return Ok(Flow::Storage { name, operations });
        }
        let source = cx.heap_mut().storage()?.read_text(&name, &[])?;
        self.loaded_source(cx, name, source)?;
        Ok(Flow::Next)
    }
    pub(super) fn loaded_source(
        &mut self,
        cx: &mut NativeCx<'_>,
        name: Text,
        source: Text,
    ) -> NativeResult<()> {
        let scenario = Arc::new(Scenario::new(source).map_err(error)?);
        cx.with_state::<State, _>(|s, _| {
            s.cache.borrow_mut().insert(name.clone(), scenario.clone());
            s.parser.load(name.clone(), scenario);
            Ok(())
        })?;
        self.commands.push(Command::Loaded(name));
        Ok(())
    }
}
