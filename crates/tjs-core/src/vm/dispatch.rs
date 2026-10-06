//! Suspended member lookup and construction. Ordinary member/function calls do
//! not allocate continuations; only operations with another phase need one.
use super::calls::{Invocation, ReturnTo};
use super::{CallError, Vm};
use crate::{
    FunctionKind, Heap, ObjId, ObjRef, Value,
    member::{self, MemberError},
};

#[derive(Clone, Copy)]
pub(super) enum ReadMode {
    Flags(crate::MemberFlags),
    Value,
    Optional,
    OptionalOr(Value),
    RawOptional(Value),
    RequiredOr(Value),
    Required,
    Raw,
    TypeOf,
}

impl ReadMode {
    pub(super) fn member_mode(self) -> member::GetMode {
        match self {
            Self::Flags(flags) => member::GetMode::Flags(flags),
            Self::Value | Self::Optional | Self::OptionalOr(_) => member::GetMode::Value,
            Self::Raw | Self::RawOptional(_) => member::GetMode::Raw,
            Self::Required | Self::RequiredOr(_) | Self::TypeOf => member::GetMode::Required,
        }
    }

    fn missing_value(self) -> Value {
        match self {
            Self::RawOptional(value) | Self::OptionalOr(value) | Self::RequiredOr(value) => value,
            _ => Value::Void,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Access {
    Read {
        destination: ReturnTo,
        mode: ReadMode,
    },
    Write {
        destination: ReturnTo,
        value: Value,
        ensure: bool,
        raw: bool,
        hidden: bool,
        class_only: bool,
        ignore_invalid: bool,
    },
    Update {
        destination: ReturnTo,
        key: Value,
        value: Value,
        op: crate::value::UpdateOp,
    },
    Contains {
        destination: ReturnTo,
    },
    Delete {
        destination: ReturnTo,
    },
    Call {
        invocation: Invocation,
        instance: Option<ObjId>,
    },
}

pub(super) struct Lookup {
    pub(super) receiver: Value,
    pub(super) key: Value,
    pub(super) access: Access,
    fallback: Option<Value>,
    // Class proxies search bases in reverse declaration order.
    path: Vec<(Value, usize)>,
    ensure_target: Value,
    pub(super) advance: bool,
}

impl Lookup {
    pub(super) fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        let (value, this, instance) = match self.access {
            Access::Read {
                mode:
                    ReadMode::RawOptional(value)
                    | ReadMode::OptionalOr(value)
                    | ReadMode::RequiredOr(value),
                ..
            } => (value, None, None),
            Access::Write { value, .. } | Access::Update { value, .. } => (value, None, None),
            Access::Call {
                invocation,
                instance,
            } => (Value::Void, invocation.this, instance),
            _ => (Value::Void, None, None),
        };
        [
            self.receiver,
            self.key,
            self.ensure_target,
            self.fallback.unwrap_or(Value::Void),
            value,
            this.map_or(Value::Void, |id| Value::Obj(id.into())),
            instance.map_or(Value::Void, |id| Value::Obj(id.into())),
        ]
        .into_iter()
        .chain(self.path.iter().map(|&(value, _)| value))
    }
}

pub(super) enum Action {
    Update(Box<super::updates::PendingUpdate>),
    Exception(super::errors::BuildException),
    Missing(Box<super::missing::PendingMissing>),
    Native(Box<super::native::PendingNative>),
    TryNative(Box<super::native::PendingTryNative>),
    Finalize {
        guard: crate::heap::lifecycle::Finalization,
        destination: ReturnTo,
        getter: bool,
    },
    TypeOf {
        destination: ReturnTo,
    },
    Lookup(Lookup),
    Invoke {
        invocation: Invocation,
        instance: Option<ObjId>,
    },
    Construct {
        class: ObjId,
        instance: ObjId,
        invocation: Invocation,
    },
    ConstructorReturn {
        invocation: Invocation,
    },
    Instance {
        instance: ObjId,
        invocation: Invocation,
    },
}

impl Action {
    pub(super) fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        let mut roots = [Value::Void; 6];
        let mut native_roots = smallvec::SmallVec::<[Value; 8]>::new();
        let mut lookup_roots = None;
        let invocation = match self {
            Self::Update(pending) => {
                for (slot, value) in roots.iter_mut().zip(pending.roots()) {
                    *slot = value;
                }
                None
            }
            Self::Missing(pending) => {
                roots[0] = pending.property;
                lookup_roots = Some(&pending.lookup);
                None
            }
            Self::Exception(build) => {
                roots[..3].copy_from_slice(&[build.value, build.message, build.trace]);
                None
            }
            Self::Native(pending) => {
                roots[0] = Value::Obj(pending.context.into());
                pending
                    .continuation
                    .trace(&mut |value| native_roots.push(value));
                Some(pending.invocation)
            }
            Self::TryNative(pending) => {
                roots[0] = Value::Obj(pending.context.into());
                roots[1] = pending.error.unwrap_or(Value::Void);
                pending
                    .continuation
                    .trace(&mut |value| native_roots.push(value));
                Some(pending.invocation)
            }
            Self::Finalize { guard, .. } => {
                roots[0] = Value::Obj(guard.object.into());
                None
            }
            Self::TypeOf { .. } => None,
            Self::Lookup(lookup) => {
                lookup_roots = Some(lookup);
                None
            }
            Self::Invoke {
                invocation,
                instance,
            } => {
                roots[0] = instance.map_or(Value::Void, |id| Value::Obj(id.into()));
                Some(*invocation)
            }
            Self::Construct {
                class,
                instance,
                invocation,
            } => {
                roots[0] = Value::Obj((*class).into());
                roots[1] = Value::Obj((*instance).into());
                Some(*invocation)
            }
            Self::ConstructorReturn { invocation } => Some(*invocation),
            Self::Instance {
                instance,
                invocation,
            } => {
                roots[0] = Value::Obj((*instance).into());
                Some(*invocation)
            }
        };
        if let Some(invocation) = invocation {
            roots[5] = invocation
                .this
                .map_or(Value::Void, |id| Value::Obj(id.into()));
        }
        roots
            .into_iter()
            .chain(lookup_roots.into_iter().flat_map(Lookup::roots))
            .chain(native_roots)
    }
}

pub(super) struct Continuation {
    pub depth: usize,
    pub action: Action,
}

impl Vm {
    // These are dispatch statuses produced before a script body executes.
    // Never catch Native/Conversion errors here: they may come from a getter.
    pub(super) fn ignore_member_status(
        &mut self,
        access: Access,
        error: &MemberError,
        advance: bool,
    ) -> bool {
        if !matches!(
            error,
            MemberError::Missing
                | MemberError::AccessDenied
                | MemberError::NotProperty
                | MemberError::NotObject
                | MemberError::NullObject
                | MemberError::Heap(crate::HeapError::InvalidObject)
        ) {
            return false;
        }
        match access {
            Access::Read {
                destination,
                mode:
                    ReadMode::OptionalOr(fallback)
                    | ReadMode::RawOptional(fallback)
                    | ReadMode::RequiredOr(fallback),
            } => {
                self.deliver(destination, fallback);
                if advance {
                    self.frame.pc += 1;
                }
                true
            }
            Access::Call { invocation, .. } if invocation.destination.ignores_status() => {
                self.complete(invocation, Value::Void, advance);
                true
            }
            Access::Write { destination, .. } if destination.ignores_status() => {
                self.deliver(destination, Value::Void);
                if advance {
                    self.frame.pc += 1;
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn push_action(&mut self, action: Action) {
        self.continuations.push(Continuation {
            depth: self.callers.len() + 1,
            action,
        });
    }

    pub(super) fn name_receivers(&mut self, heap: &mut Heap) -> (Value, Option<Value>) {
        let global = self.ensure_global(heap);
        let this = self.frame.this.unwrap_or(global);
        (
            Value::Obj(ObjRef::bound(this)),
            (this != global).then_some(Value::Obj(ObjRef {
                object: Some(global),
                this: Some(this),
            })),
        )
    }

    pub(super) fn access(
        &mut self,
        heap: &mut Heap,
        receiver: Value,
        key: Value,
        access: Access,
        fallback: Option<Value>,
        advance: bool,
    ) -> Result<(), CallError> {
        // Existing members take the ordinary path without building a proxy
        // search. Missing stores are resolved below before ensuring a member.
        let (receiver, fallback) = if let Some((this, global)) = heap.scope(receiver)? {
            let Value::Obj(reference) = receiver else {
                unreachable!("scope object")
            };
            let context = reference.this.unwrap_or(this);
            (
                Value::Obj(ObjRef {
                    object: Some(this),
                    this: Some(context),
                }),
                Some(Value::Obj(ObjRef {
                    object: Some(global),
                    this: Some(context),
                })),
            )
        } else {
            (receiver, fallback)
        };
        let result = match access {
            Access::Update { .. } => member::update_value(heap, receiver, key),
            Access::Read { mode, .. } => member::get_mode(heap, receiver, key, mode.member_mode()),
            Access::Write {
                value,
                ensure,
                raw: true,
                hidden,
                class_only,
                ..
            } => member::set_flags(
                heap,
                receiver,
                key,
                value,
                !ensure,
                true,
                (hidden, class_only),
            )
            .map(|()| Value::Void),
            Access::Write {
                value,
                hidden,
                class_only,
                ..
            } => member::set_flags(
                heap,
                receiver,
                key,
                value,
                true,
                false,
                (hidden, class_only),
            )
            .map(|()| Value::Void),
            Access::Contains { .. } => member::contains(heap, receiver, key),
            Access::Call { .. } => member::callable(heap, receiver, key),
            Access::Delete { .. } => member::delete_lookup(heap, receiver, key),
        };
        match result {
            Ok(value) => return self.access_result(heap, access, receiver, value, advance),
            Err(MemberError::Heap(crate::HeapError::InvalidObject))
                if matches!(
                    access,
                    Access::Write {
                        ignore_invalid: true,
                        ..
                    }
                ) =>
            {
                return self.access_result(heap, access, receiver, Value::Void, advance);
            }
            Err(MemberError::Missing)
                if matches!(
                    access,
                    Access::Read {
                        mode: ReadMode::Optional
                            | ReadMode::RawOptional(_)
                            | ReadMode::OptionalOr(_)
                            | ReadMode::RequiredOr(_),
                        ..
                    }
                ) && matches!(receiver, Value::Str(_) | Value::Octet(_)) =>
            {
                let Access::Read { mode, .. } = access else {
                    unreachable!("optional read checked above")
                };
                return self.access_result(heap, access, receiver, mode.missing_value(), advance);
            }
            Err(MemberError::AccessDenied) if self.ignore_constructor_status(access, advance) => {
                return Ok(());
            }
            // Primitive property helpers throw directly, including missing names
            // under typeof; object proxy fallback/undefined rules do not apply.
            Err(error) if matches!(receiver, Value::Str(_) | Value::Octet(_)) => {
                return Err(error.into());
            }
            Err(MemberError::MissingHook {
                object,
                name,
                value,
            }) => {
                return self.call_missing(
                    heap,
                    Lookup {
                        receiver,
                        key,
                        access,
                        fallback,
                        path: Vec::new(),
                        ensure_target: receiver,
                        advance,
                    },
                    object,
                    name,
                    value,
                );
            }
            // Resolution already selected the accessor and its context. No
            // script has run yet, so enter it without repeating member lookup.
            Err(MemberError::Invoke { function, argument }) => {
                return self.accessor_result(heap, access, receiver, function, argument, advance);
            }
            Err(MemberError::Missing) => {}
            Err(error) if self.ignore_member_status(access, &error, advance) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        self.lookup(
            heap,
            Lookup {
                receiver,
                key,
                access,
                fallback,
                path: Vec::new(),
                ensure_target: receiver,
                advance,
            },
        )
    }

    pub(super) fn lookup(&mut self, heap: &mut Heap, mut lookup: Lookup) -> Result<(), CallError> {
        loop {
            let Lookup {
                receiver,
                key,
                access,
                ..
            } = lookup;
            let class = match receiver {
                Value::Obj(reference) => reference
                    .object
                    .and_then(|id| heap.function(id).ok().flatten())
                    .is_some_and(|function| matches!(function.kind(), FunctionKind::Class { .. })),
                _ => false,
            };
            let result = match access {
                Access::Update { .. } => member::update_value(heap, receiver, key),
                Access::Read { mode, .. } => {
                    member::get_mode(heap, receiver, key, mode.member_mode())
                }
                Access::Write {
                    value,
                    ensure,
                    raw,
                    hidden,
                    class_only,
                    ..
                } => if raw {
                    member::set_flags(
                        heap,
                        receiver,
                        key,
                        value,
                        !ensure,
                        true,
                        (hidden, class_only),
                    )
                } else if ensure && !class && lookup.path.is_empty() {
                    member::set_flags(
                        heap,
                        receiver,
                        key,
                        value,
                        false,
                        false,
                        (hidden, class_only),
                    )
                } else {
                    member::set_flags(
                        heap,
                        receiver,
                        key,
                        value,
                        true,
                        false,
                        (hidden, class_only),
                    )
                }
                .map(|()| Value::Void),
                Access::Delete { .. } => member::delete_lookup(heap, receiver, key),
                Access::Contains { .. } => member::contains(heap, receiver, key),
                Access::Call { .. } => member::callable(heap, receiver, key),
            };
            match result {
                Err(MemberError::Heap(crate::HeapError::InvalidObject))
                    if matches!(
                        access,
                        Access::Write {
                            ignore_invalid: true,
                            ..
                        }
                    ) =>
                {
                    return self.access_result(heap, access, receiver, Value::Void, lookup.advance);
                }
                Ok(value) => {
                    return self.access_result(heap, access, receiver, value, lookup.advance);
                }
                Err(MemberError::Invoke { function, argument }) => {
                    return self.accessor_result(
                        heap,
                        access,
                        receiver,
                        function,
                        argument,
                        lookup.advance,
                    );
                }
                Err(MemberError::MissingHook {
                    object,
                    name,
                    value,
                }) => {
                    return self.call_missing(heap, lookup, object, name, value);
                }
                Err(MemberError::Missing) => {}
                Err(MemberError::AccessDenied)
                    if self.ignore_constructor_status(access, lookup.advance) =>
                {
                    return Ok(());
                }
                Err(error) if self.ignore_member_status(access, &error, lookup.advance) => {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
            // A failing ordinary store (for example a negative Array index)
            // must not be retried as a raw member declaration.
            if matches!(access, Access::Write { ensure: true, .. })
                && !class
                && lookup.path.is_empty()
            {
                return Err(MemberError::Missing.into());
            }
            if class {
                let Value::Obj(reference) = receiver else {
                    unreachable!()
                };
                let function = heap
                    .function(reference.object.expect("class"))?
                    .expect("class");
                let FunctionKind::Class { bases, .. } = function.kind() else {
                    unreachable!()
                };
                if !bases.is_empty() {
                    if lookup.path.len() >= self.limits.max_call_depth {
                        return Err(CallError::Depth);
                    }
                    lookup.path.push((receiver, bases.len()));
                }
            }
            while let Some((class, remaining)) = lookup.path.last_mut() {
                if *remaining == 0 {
                    lookup.path.pop();
                    continue;
                }
                *remaining -= 1;
                let Value::Obj(reference) = *class else {
                    unreachable!()
                };
                let function = heap
                    .function(reference.object.expect("class"))?
                    .expect("class");
                let FunctionKind::Class { bases, .. } = function.kind() else {
                    unreachable!()
                };
                let mut resolver = heap.function_value(function.pool, bases[*remaining]);
                if let Value::Obj(reference) = &mut resolver {
                    reference.this = Some(function.global);
                }
                let advance = lookup.advance;
                lookup.advance = false;
                self.push_action(Action::Lookup(lookup));
                return self.invoke_accessor(heap, resolver, None, ReturnTo::Resume, advance);
            }
            if let Some(fallback) = lookup.fallback.take() {
                lookup.receiver = fallback;
                continue;
            }
            match access {
                Access::Read {
                    destination,
                    mode:
                        mode @ (ReadMode::Optional
                        | ReadMode::RawOptional(_)
                        | ReadMode::OptionalOr(_)
                        | ReadMode::RequiredOr(_)),
                } => {
                    self.deliver(destination, mode.missing_value());
                    if lookup.advance {
                        self.frame.pc += 1;
                    }
                    return Ok(());
                }
                Access::Write {
                    destination,
                    value,
                    ensure: true,
                    hidden,
                    class_only,
                    ..
                } => {
                    member::define(heap, lookup.ensure_target, key, value)?;
                    if hidden || class_only {
                        member::set_flags(
                            heap,
                            lookup.ensure_target,
                            key,
                            value,
                            true,
                            true,
                            (hidden, class_only),
                        )?;
                    }
                    self.deliver(destination, Value::Void);
                    if lookup.advance {
                        self.frame.pc += 1;
                    }
                    return Ok(());
                }
                Access::Delete { destination } | Access::Contains { destination } => {
                    self.deliver(destination, Value::Int(0));
                    if lookup.advance {
                        self.frame.pc += 1;
                    }
                    return Ok(());
                }
                Access::Call {
                    invocation,
                    instance: Some(instance),
                } => {
                    self.complete(
                        invocation,
                        Value::Obj(ObjRef::bound(instance)),
                        lookup.advance,
                    );
                    return Ok(());
                }
                Access::Read {
                    mode: ReadMode::TypeOf,
                    ..
                } => {
                    return self.undefined_result(heap, access, lookup.advance);
                }
                _ if self.ignore_member_status(access, &MemberError::Missing, lookup.advance) => {
                    return Ok(());
                }
                _ => return Err(MemberError::Missing.into()),
            }
        }
    }

    pub(super) fn accessor_result(
        &mut self,
        heap: &mut Heap,
        access: Access,
        receiver: Value,
        function: Value,
        argument: Option<Value>,
        advance: bool,
    ) -> Result<(), CallError> {
        let destination = match access {
            Access::Update { .. } => unreachable!("update accessors use their retained target"),
            Access::Read { destination, mode } => {
                if matches!(mode, ReadMode::TypeOf) {
                    self.push_action(Action::TypeOf { destination });
                    ReturnTo::Resume
                } else {
                    destination
                }
            }
            Access::Write { destination, .. } => destination,
            Access::Call {
                mut invocation,
                instance,
            } => {
                if let Value::Obj(reference) = receiver {
                    invocation.this = reference.this;
                }
                self.push_action(Action::Invoke {
                    invocation,
                    instance,
                });
                ReturnTo::Resume
            }
            Access::Delete { .. } | Access::Contains { .. } => {
                unreachable!("presence and deletion bypass properties")
            }
        };
        self.invoke_accessor(heap, function, argument, destination, advance)
    }

    fn undefined_result(
        &mut self,
        heap: &mut Heap,
        access: Access,
        advance: bool,
    ) -> Result<(), CallError> {
        let Access::Read { destination, .. } = access else {
            unreachable!("typeof member")
        };
        let value = self.type_name(heap, None);
        self.deliver(destination, value);
        if advance {
            self.frame.pc += 1;
        }
        Ok(())
    }

    pub(super) fn access_result(
        &mut self,
        heap: &mut Heap,
        access: Access,
        receiver: Value,
        value: Value,
        advance: bool,
    ) -> Result<(), CallError> {
        match access {
            Access::Update { .. } => {
                return self.update_result(heap, receiver, access, value, false, advance);
            }
            Access::Read { destination, mode } => {
                let value = if matches!(mode, ReadMode::TypeOf) {
                    self.type_name(heap, Some(value))
                } else {
                    value
                };
                self.deliver(destination, value);
            }
            Access::Delete { destination } | Access::Contains { destination } => {
                self.deliver(destination, value)
            }
            Access::Write { destination, .. } => self.deliver(destination, Value::Void),
            Access::Call {
                mut invocation,
                instance,
            } => {
                if let Value::Obj(reference) = receiver {
                    invocation.this = reference.this;
                }
                return self.invoke_member(heap, value, invocation, instance, advance);
            }
        }
        if advance {
            self.frame.pc += 1;
        }
        Ok(())
    }

    fn invoke_member(
        &mut self,
        heap: &mut Heap,
        value: Value,
        mut invocation: Invocation,
        instance: Option<ObjId>,
        advance: bool,
    ) -> Result<(), CallError> {
        if invocation.destination.ignores_status() {
            // Preflight only. Errors from invoke/native_step must propagate,
            // even when they have the same shape as a dispatch failure.
            let callable = if let Value::Obj(reference) = value {
                if let Some(object) = reference.object {
                    heap.is_valid(object)?
                        && (heap.object(object)?.kind() == crate::ObjectKind::NativeClass
                            || heap.native_callable(object, false)?.is_some()
                            || heap.function(object)?.is_some_and(|f| {
                                !matches!(f.kind(), FunctionKind::Property { .. })
                            }))
                } else {
                    false
                }
            } else {
                false
            };
            if !callable {
                self.complete(invocation, Value::Void, advance);
                return Ok(());
            }
        }
        if let Some(instance) = instance {
            self.push_action(Action::Instance {
                instance,
                invocation,
            });
            invocation.destination = ReturnTo::ResumeDiscard;
        }
        let result = self.invoke(heap, value, invocation, false, advance);
        // CreateNew ignores a non-callable or absent named constructor. Script
        // exceptions still propagate from constructors that actually execute.
        if matches!(result, Err(CallError::NotCallable)) && instance.is_some() {
            let Action::Instance {
                instance,
                invocation,
            } = self.continuations.pop().expect("constructor").action
            else {
                unreachable!()
            };
            self.complete(invocation, Value::Obj(ObjRef::bound(instance)), advance);
            Ok(())
        } else {
            result
        }
    }

    fn ignore_constructor_status(&mut self, access: Access, advance: bool) -> bool {
        if let Access::Call {
            invocation,
            instance: Some(instance),
        } = access
        {
            self.complete(invocation, Value::Obj(ObjRef::bound(instance)), advance);
            true
        } else {
            false
        }
    }

    pub(super) fn resume(&mut self, heap: &mut Heap, value: Value) -> Result<(), CallError> {
        let action = self
            .continuations
            .pop()
            .expect("pending continuation")
            .action;
        match action {
            Action::Update(pending) => self.resume_update(heap, *pending, value),
            Action::Missing(pending) => self.resume_missing(heap, *pending, value),
            Action::Exception(build) => self.resume_exception(heap, build, value),
            Action::Native(pending) => {
                let step = pending.continuation.resume(
                    &mut crate::NativeCx::new(
                        heap,
                        pending.context,
                        pending.invocation.destination.result_needed(),
                    ),
                    value,
                )?;
                self.native_step(heap, pending.context, pending.invocation, step, false)
            }
            Action::TryNative(pending) => {
                let result = pending.error.map_or(Ok(value), Err);
                let step = pending.continuation.resume(
                    &mut crate::NativeCx::new(
                        heap,
                        pending.context,
                        pending.invocation.destination.result_needed(),
                    ),
                    result,
                )?;
                self.native_step(heap, pending.context, pending.invocation, step, false)
            }
            Action::Finalize {
                guard,
                destination,
                getter,
            } => {
                if getter {
                    self.call_finalizer(heap, guard, destination, value, false)
                } else {
                    self.finish_invalidation(heap, guard, destination, false)
                }
            }
            Action::TypeOf { destination } => {
                let value = self.type_name(heap, Some(value));
                self.deliver(destination, value);
                Ok(())
            }
            Action::Lookup(mut lookup) => {
                let Value::Obj(mut reference) = value else {
                    return Err(MemberError::NotObject.into());
                };
                let Some(&(Value::Obj(class), _)) = lookup.path.last() else {
                    unreachable!("base resolver")
                };
                reference.this = reference.this.or(class.this);
                lookup.receiver = Value::Obj(reference);
                self.lookup(heap, lookup)
            }
            Action::Invoke {
                invocation,
                instance,
            } => {
                // TJSDefaultFuncCall converts a property's successful result
                // to an object closure. That conversion throws for primitives;
                // it is not the status of a directly stored non-callable slot.
                if invocation.destination.ignores_status() && !matches!(value, Value::Obj(_)) {
                    return Err(crate::NativeError::Type(
                        "an object returned by the property getter",
                    )
                    .into());
                }
                self.invoke_member(heap, value, invocation, instance, false)
            }
            Action::Construct {
                class,
                instance,
                invocation,
            } => {
                let function = heap.function(class)?.expect("class");
                let FunctionKind::Class { constructor, .. } = *function.kind() else {
                    unreachable!("class")
                };
                // The initializer's frame has returned, so its module may have
                // left the active VM table. The class still owns its constants.
                let key = heap.constant_value(heap.function_constants(function.pool), constructor);
                self.access(
                    heap,
                    Value::Obj(ObjRef {
                        object: Some(class),
                        this: Some(instance),
                    }),
                    key,
                    Access::Call {
                        invocation,
                        instance: Some(instance),
                    },
                    None,
                    false,
                )
            }
            Action::ConstructorReturn { invocation } => {
                self.complete(invocation, Value::Void, false);
                Ok(())
            }
            Action::Instance {
                instance,
                invocation,
            } => {
                self.complete(invocation, Value::Obj(ObjRef::bound(instance)), false);
                Ok(())
            }
        }
    }

    pub(super) fn register_members(&mut self, heap: &mut Heap) -> Result<(), CallError> {
        let class = if let Some(class) = self.frame.callee {
            class
        } else {
            let Value::Obj(reference) =
                self.load_function(heap, crate::FunctionId(self.frame.function as u32))
            else {
                unreachable!()
            };
            reference.object.expect("class")
        };
        let instance = self.this(heap);
        heap.copy_script_members(class, instance)?;
        Ok(())
    }
}
