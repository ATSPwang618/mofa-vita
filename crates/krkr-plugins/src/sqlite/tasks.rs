use super::*;
#[derive(Clone, Copy, tjs_bind::Trace)]
pub(super) enum Simple {
    Close,
    Reset,
    Exec,
    Step,
}
#[derive(tjs_bind::Trace)]
enum Finish {
    Bool,
    Value,
    Code,
    Step,
    Void,
    Construct(Value),
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Stage {
    Prepared,
    Bound,
    Stepping,
    Simple,
}
#[derive(tjs_bind::Trace)]
struct Job {
    owner: ObjId,
    target: Option<ObjId>,
    #[trace(skip = "Database lease and statement identity have no VM objects")]
    lease: Arc<Lease>,
    params: Option<Value>,
    callback: Option<Value>,
    execute: bool,
    first: bool,
    finish: Finish,
    stage: Stage,
}
fn lease(cx: &mut NativeCx<'_>, owner: ObjId) -> NativeResult<Arc<Lease>> {
    statement::with_state(cx, owner, |s| s.lease.clone())?.ok_or(NativeError::This)
}
fn request(cx: &mut NativeCx<'_>, job: Box<Job>, command: Command) -> NativeResult<NativeStep> {
    let db = job.lease.db.clone();
    extensions::run_work(cx, move |stop| db.request(command, stop), job)
}
pub(super) fn execute(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    first: bool,
) -> NativeResult<NativeStep> {
    let text = sql(cx, crate::exports::arg(args, 0)?)?;
    let owner = cx.this();
    let db = database::with_state(cx, owner, |s| s.db.clone())?.ok_or(NativeError::This)?;
    let lease = Arc::new(Lease {
        id: db.statement_id(),
        db,
    });
    let id = lease.id;
    let job = Box::new(Job {
        owner,
        target: None,
        lease,
        params: args.get(1).copied(),
        callback: args.get(2).copied().filter(|v| matches!(v, Value::Obj(_))),
        execute: true,
        first,
        finish: if first { Finish::Value } else { Finish::Bool },
        stage: Stage::Prepared,
    });
    request(cx, job, Command::Prepare(id, text))
}
pub(super) fn open(
    cx: &mut NativeCx<'_>,
    owner: ObjId,
    args: &[Value],
    constructor: Option<Value>,
) -> NativeResult<NativeStep> {
    let text = sql(cx, crate::exports::arg(args, 0)?)?;
    let lease = lease(cx, owner)?;
    let id = lease.id;
    statement::with_state(cx, owner, |s| s.snapshot = Snapshot::default())?;
    let job = Box::new(Job {
        owner,
        target: Some(owner),
        lease,
        params: args.get(1).copied(),
        callback: None,
        execute: false,
        first: false,
        finish: constructor.map_or(Finish::Code, Finish::Construct),
        stage: Stage::Prepared,
    });
    request(cx, job, Command::Prepare(id, text))
}
pub(super) fn simple(cx: &mut NativeCx<'_>, action: Simple) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let lease = lease(cx, owner)?;
    let id = lease.id;
    let (command, finish) = match action {
        Simple::Close => (Command::Close(id), Finish::Void),
        Simple::Reset => (Command::Reset(id), Finish::Code),
        Simple::Exec => (Command::Step(id), Finish::Code),
        Simple::Step => (Command::Step(id), Finish::Step),
    };
    if matches!(action, Simple::Close) {
        statement::with_state(cx, owner, |s| s.snapshot = Snapshot::default())?;
    }
    request(
        cx,
        Box::new(Job {
            owner,
            target: Some(owner),
            lease,
            params: None,
            callback: None,
            execute: false,
            first: false,
            finish,
            stage: Stage::Simple,
        }),
        command,
    )
}
pub(super) fn transaction(cx: &mut NativeCx<'_>, sql: &'static str) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let db = database::with_state(cx, owner, |s| s.db.clone())?.ok_or(NativeError::This)?;
    request(
        cx,
        Box::new(Job {
            owner,
            target: None,
            lease: Arc::new(Lease { db, id: 0 }),
            params: None,
            callback: None,
            execute: false,
            first: false,
            finish: Finish::Bool,
            stage: Stage::Simple,
        }),
        Command::Transaction(sql),
    )
}
pub(super) fn bind(cx: &mut NativeCx<'_>, params: Value) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let lease = lease(cx, owner)?;
    Box::new(Job {
        owner,
        target: Some(owner),
        lease,
        params: None,
        callback: None,
        execute: false,
        first: false,
        finish: Finish::Code,
        stage: Stage::Bound,
    })
    .bind(cx, params)
}
pub(super) fn bind_at(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let value = from_tjs(cx, crate::exports::arg(args, 0)?)?;
    let pos = args.get(1).map(|&v| position(cx, v)).transpose()?;
    let owner = cx.this();
    let lease = lease(cx, owner)?;
    let id = lease.id;
    request(
        cx,
        Box::new(Job {
            owner,
            target: Some(owner),
            lease,
            params: None,
            callback: None,
            execute: false,
            first: false,
            finish: Finish::Code,
            stage: Stage::Simple,
        }),
        Command::BindAt(id, value, pos),
    )
}
impl Job {
    fn result(&self, code: i32) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(match self.finish {
            Finish::Bool => Value::Int((code == 0 || code == 101) as i64),
            Finish::Value | Finish::Void => Value::Void,
            Finish::Code => Value::Int(i64::from(code)),
            Finish::Step => Value::Int((code == 100) as i64),
            Finish::Construct(value) => {
                if code != 0 {
                    return Err(NativeError::Message("failed to open state"));
                }
                value
            }
        }))
    }
    fn step(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.stage = Stage::Stepping;
        let id = self.lease.id;
        request(cx, self, Command::Step(id))
    }
    fn after_bound(self: Box<Self>, cx: &mut NativeCx<'_>, code: i32) -> NativeResult<NativeStep> {
        if code == 0 && self.execute {
            self.step(cx)
        } else {
            self.result(code)
        }
    }
    fn bind(mut self: Box<Self>, cx: &mut NativeCx<'_>, source: Value) -> NativeResult<NativeStep> {
        let id = crate::exports::object(source)?;
        self.stage = Stage::Bound;
        let array = cx.heap().object(id)?.kind() == tjs_core::ObjectKind::Array
            || cx
                .heap()
                .class_names(id)?
                .iter()
                .any(|n| n.iter().copied().eq("Array".encode_utf16()));
        if array {
            let key = Value::Str(
                cx.heap_mut()
                    .alloc_string("count".encode_utf16().collect::<Vec<_>>()),
            );
            return Ok(NativeStep::GetOr {
                object: source,
                key,
                raw: false,
                fallback: Value::Void,
                continuation: Box::new(BindArray {
                    job: self,
                    source,
                    count: None,
                    index: 0,
                }),
            });
        }
        // The supplied C++ BindCaller binds param[1] (member flags), not param[2].
        let values = cx
            .heap()
            .members_with_flags(id)?
            .map(|(name, _, class_only)| {
                Ok((
                    Position::Name(String::from_utf16_lossy(cx.heap().symbol(name)?)),
                    Cell::Integer(if class_only { 65536 } else { 0 }),
                ))
            })
            .collect::<NativeResult<Vec<_>>>()?;
        let statement = self.lease.id;
        request(cx, self, Command::Bind(statement, values))
    }
}
impl extensions::WorkContinuation<backend::Reply> for Job {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        reply: backend::Reply,
    ) -> NativeResult<NativeStep> {
        if let (Some(owner), Some(snapshot)) = (self.target, reply.snapshot.as_ref()) {
            statement::with_state(cx, owner, |s| {
                if s.lease.as_ref().is_some_and(|l| l.id == self.lease.id) {
                    s.snapshot = snapshot.clone();
                }
            })?;
        }
        match self.stage {
            Stage::Prepared => {
                if reply.code != 0 {
                    return self.result(reply.code);
                }
                if let Some(params) = self.params.take() {
                    return self.bind(cx, params);
                }
                self.after_bound(cx, 0)
            }
            Stage::Bound => self.after_bound(cx, reply.code),
            Stage::Simple => self.result(reply.code),
            Stage::Stepping => {
                if reply.code != 100 {
                    return self.result(reply.code);
                }
                let row = reply.snapshot.map(|s| s.row).unwrap_or_default();
                if self.first {
                    return Ok(NativeStep::Return(to_tjs(cx, row.first())?));
                }
                if let Some(callback) = self.callback {
                    let mut arguments = Vec::with_capacity(row.len());
                    for cell in &row {
                        arguments.push(to_tjs(cx, Some(cell))?);
                    }
                    return Ok(NativeStep::CallDiscard {
                        function: callback,
                        arguments,
                        continuation: self,
                    });
                }
                self.step(cx)
            }
        }
    }
}
impl NativeContinuation for Job {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.step(cx)
    }
}
#[derive(tjs_bind::Trace)]
struct BindArray {
    job: Box<Job>,
    source: Value,
    count: Option<i32>,
    index: i32,
}
impl BindArray {
    fn next(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index >= self.count.unwrap() {
            return self.job.after_bound(cx, 0);
        }
        Ok(NativeStep::GetOr {
            object: self.source,
            key: Value::Int(i64::from(self.index)),
            raw: false,
            fallback: Value::Void,
            continuation: self,
        })
    }
}
impl NativeContinuation for BindArray {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.count.is_none() {
            let count = value::to_integer(cx.heap(), value)? as i32;
            if count > 65536 {
                return Err(NativeError::Message("SQLite binding array exceeds limit"));
            }
            self.count = Some(count);
            return self.next(cx);
        }
        let cell = from_tjs(cx, value)?;
        let command = Command::Bind(
            self.job.lease.id,
            vec![(Position::Index(self.index as usize + 1), cell)],
        );
        let db = self.job.lease.db.clone();
        extensions::run_work(cx, move |stop| db.request(command, stop), self)
    }
}
impl extensions::WorkContinuation<backend::Reply> for BindArray {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        reply: backend::Reply,
    ) -> NativeResult<NativeStep> {
        if reply.code != 0 {
            return self.job.after_bound(cx, reply.code);
        }
        self.index += 1;
        self.next(cx)
    }
}
