//! sqlite3.dll: portable database and persistent statement classes.
mod backend;
mod normalize;
mod tasks;
use backend::{Cell, Command, Database, Position, Snapshot};
use krkr_engine::{extensions, storages};
use std::sync::Arc;
use tjs_bind::{Array, IntoTjs, RestArgs, Utf16};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value, value,
};
krkr_engine::native_plugin! { pub(crate) Sqlite {names:["sqlite3.dll","sqlite3.tpm"], classes:[database,statement],extensions:[]} }
struct Lease {
    db: Arc<Database>,
    id: u64,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.db.close(self.id);
    }
}
#[tjs_bind::class(name = "Sqlite")]
mod database {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) db: Option<Arc<Database>>,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::constructor(resumable = true)]
        fn create(
            cx: &mut NativeCx<'_>,
            name: Utf16,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let readonly = args
                .first()
                .map(|v| value::to_integer(cx.heap(), *v))
                .transpose()?
                .unwrap_or(0) as i32
                != 0;
            let name = tjs_core::string::c_string(&name.0);
            if readonly {
                return storages::managed::plans(
                    cx,
                    vec![(name.to_vec(), false)],
                    Opened { owner: cx.this() },
                    |next, cx, mut plans| {
                        let input = plans.pop().flatten().map_or_else(
                            || backend::Input::Failed("database storage not found".into()),
                            backend::Input::Archive,
                        );
                        extensions::run_work(
                            cx,
                            move |stop| Database::open(input, stop),
                            Box::new(next),
                        )
                    },
                );
            }
            let input = if name.is_empty() || name[0] == 58 {
                backend::Input::Special(String::from_utf16_lossy(name))
            } else {
                let vfs = storages::service(cx)?;
                let path = vfs
                    .borrow()
                    .local_name(name)
                    .map_err(|e| NativeError::Detail(e.to_string()))?;
                backend::Input::Local(std::path::PathBuf::from(String::from_utf16_lossy(&path)))
            };
            extensions::run_work(
                cx,
                move |stop| Database::open(input, stop),
                Box::new(Opened { owner: cx.this() }),
            )
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.db = None;
        }
        #[tjs::method(name = "exec", resumable = true)]
        fn exec(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            tasks::execute(cx, args, false)
        }
        #[tjs::method(name = "execValue", resumable = true)]
        fn exec_value(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            tasks::execute(cx, args, true)
        }
        #[tjs::method(name = "begin", resumable = true)]
        fn begin(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::transaction(cx, "BEGIN TRANSACTION;")
        }
        #[tjs::method(name = "commit", resumable = true)]
        fn commit(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::transaction(cx, "COMMIT;")
        }
        #[tjs::method(name = "rollback", resumable = true)]
        fn rollback(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::transaction(cx, "ROLLBACK;")
        }
        #[tjs::getter(name = "lastInsertRowId")]
        fn rowid(&self) -> i64 {
            self.db
                .as_ref()
                .map_or(0, |d| d.status.lock().unwrap().rowid)
        }
        #[tjs::getter(name = "errorCode")]
        fn error_code(&self) -> i64 {
            self.db
                .as_ref()
                .map_or(-1, |d| i64::from(d.status.lock().unwrap().code))
        }
        #[tjs::getter(name = "errorMessage")]
        fn error_message(&self) -> Utf16 {
            Utf16(
                self.db
                    .as_ref()
                    .map_or_else(
                        || "database open failed".into(),
                        |d| d.status.lock().unwrap().message.clone(),
                    )
                    .encode_utf16()
                    .collect(),
            )
        }
        #[tjs::constant(name = "SQLITE_OK")]
        const OK: i64 = 0;
        #[tjs::constant(name = "SQLITE_ERROR")]
        const ERROR: i64 = 1;
        #[tjs::constant(name = "SQLITE_INTERNAL")]
        const INTERNAL: i64 = 2;
        #[tjs::constant(name = "SQLITE_PERM")]
        const PERM: i64 = 3;
        #[tjs::constant(name = "SQLITE_ABORT")]
        const ABORT: i64 = 4;
        #[tjs::constant(name = "SQLITE_BUSY")]
        const BUSY: i64 = 5;
        #[tjs::constant(name = "SQLITE_LOCKED")]
        const LOCKED: i64 = 6;
        #[tjs::constant(name = "SQLITE_NOMEM")]
        const NOMEM: i64 = 7;
        #[tjs::constant(name = "SQLITE_READONLY")]
        const READONLY: i64 = 8;
        #[tjs::constant(name = "SQLITE_INTERRUPT")]
        const INTERRUPT: i64 = 9;
        #[tjs::constant(name = "SQLITE_IOERR")]
        const IOERR: i64 = 10;
        #[tjs::constant(name = "SQLITE_CORRUPT")]
        const CORRUPT: i64 = 11;
        #[tjs::constant(name = "SQLITE_NOTFOUND")]
        const NOTFOUND: i64 = 12;
        #[tjs::constant(name = "SQLITE_FULL")]
        const FULL: i64 = 13;
        #[tjs::constant(name = "SQLITE_CANTOPEN")]
        const CANTOPEN: i64 = 14;
        #[tjs::constant(name = "SQLITE_PROTOCOL")]
        const PROTOCOL: i64 = 15;
        #[tjs::constant(name = "SQLITE_EMPTY")]
        const EMPTY: i64 = 16;
        #[tjs::constant(name = "SQLITE_SCHEMA")]
        const SCHEMA: i64 = 17;
        #[tjs::constant(name = "SQLITE_TOOBIG")]
        const TOOBIG: i64 = 18;
        #[tjs::constant(name = "SQLITE_CONSTRAINT")]
        const CONSTRAINT: i64 = 19;
        #[tjs::constant(name = "SQLITE_MISMATCH")]
        const MISMATCH: i64 = 20;
        #[tjs::constant(name = "SQLITE_MISUSE")]
        const MISUSE: i64 = 21;
        #[tjs::constant(name = "SQLITE_NOLFS")]
        const NOLFS: i64 = 22;
        #[tjs::constant(name = "SQLITE_AUTH")]
        const AUTH: i64 = 23;
        #[tjs::constant(name = "SQLITE_FORMAT")]
        const FORMAT: i64 = 24;
        #[tjs::constant(name = "SQLITE_RANGE")]
        const RANGE: i64 = 25;
        #[tjs::constant(name = "SQLITE_NOTADB")]
        const NOTADB: i64 = 26;
        #[tjs::constant(name = "SQLITE_ROW")]
        const ROW: i64 = 100;
        #[tjs::constant(name = "SQLITE_DONE")]
        const DONE: i64 = 101;
    }
}
#[derive(tjs_bind::Trace)]
struct Opened {
    owner: ObjId,
}
impl extensions::WorkContinuation<Arc<Database>> for Opened {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        db: Arc<Database>,
    ) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(
            cx.construct(database::State { db: Some(db) })?,
        ))
    }
}
#[tjs_bind::class(name = "SqliteStatement")]
mod statement {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) database: Value,
        pub(super) lease: Option<Arc<Lease>>,
        pub(super) snapshot: Snapshot,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.database.trace(visit);
        }
    }
    impl State {
        #[tjs::constructor(resumable = true)]
        fn create(
            cx: &mut NativeCx<'_>,
            database: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let id = crate::exports::object(database)?;
            let db =
                super::database::with_state(cx, id, |s| s.db.clone())?.ok_or(NativeError::This)?;
            let lease = Arc::new(Lease {
                id: db.statement_id(),
                db,
            });
            let object = cx.construct(Self {
                database,
                lease: Some(lease),
                snapshot: Snapshot::default(),
            })?;
            let owner = crate::exports::object(object)?;
            cx.heap_mut().set_call_missing(owner)?;
            if args.is_empty() {
                return Ok(NativeStep::Return(object));
            }
            tasks::open(cx, owner, args, Some(object))
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.lease = None;
            self.database = Value::Void;
            self.snapshot = Snapshot::default();
        }
        #[tjs::method(name = "open", resumable = true)]
        fn open(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            tasks::open(cx, cx.this(), args, None)
        }
        #[tjs::method(name = "close", resumable = true)]
        fn close(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::simple(cx, tasks::Simple::Close)
        }
        #[tjs::method(name = "reset", resumable = true)]
        fn reset(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::simple(cx, tasks::Simple::Reset)
        }
        #[tjs::method(name = "bind", resumable = true)]
        fn bind(cx: &mut NativeCx<'_>, params: Value) -> NativeResult<NativeStep> {
            tasks::bind(cx, params)
        }
        #[tjs::method(name = "bindAt", resumable = true)]
        fn bind_at(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            tasks::bind_at(cx, args)
        }
        #[tjs::method(name = "exec", resumable = true)]
        fn exec(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::simple(cx, tasks::Simple::Exec)
        }
        #[tjs::method(name = "step", resumable = true)]
        fn step(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            tasks::simple(cx, tasks::Simple::Step)
        }
        #[tjs::getter(name = "sql")]
        fn sql(&self) -> Utf16 {
            Utf16(self.snapshot.sql.encode_utf16().collect())
        }
        #[tjs::getter(name = "count")]
        fn count(&self) -> i64 {
            self.snapshot.row.len() as i64
        }
        #[tjs::getter(name = "columnCount")]
        fn columns(&self) -> i64 {
            self.snapshot.names.len() as i64
        }
        #[tjs::method(name = "getName")]
        fn name(&self, cx: &mut NativeCx<'_>, column: Value) -> NativeResult<Utf16> {
            let n = column_index(cx, &self.snapshot, column)?;
            Ok(Utf16(
                self.snapshot
                    .names
                    .get(n)
                    .map_or("", String::as_str)
                    .encode_utf16()
                    .collect(),
            ))
        }
        #[tjs::method(name = "getType")]
        fn get_type(&self, cx: &mut NativeCx<'_>, column: Value) -> NativeResult<i64> {
            Ok(cell_type(self.snapshot.row.get(column_index(
                cx,
                &self.snapshot,
                column,
            )?)))
        }
        #[tjs::method(name = "isNull")]
        fn null(&self, cx: &mut NativeCx<'_>, column: Value) -> NativeResult<bool> {
            Ok(cell_type(
                self.snapshot
                    .row
                    .get(column_index(cx, &self.snapshot, column)?),
            ) == 5)
        }
        #[tjs::method(name = "get")]
        fn get(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<Value> {
            if !cx.result_needed() {
                return Ok(Value::Void);
            }
            if args.is_empty() {
                let mut row = Vec::with_capacity(self.snapshot.names.len());
                for i in 0..self.snapshot.names.len() {
                    row.push(to_tjs(cx, self.snapshot.row.get(i))?);
                }
                return Array(row).into_tjs(cx.heap_mut());
            }
            let n = column_index(cx, &self.snapshot, args[0])?;
            let cell = self.snapshot.row.get(n);
            if cell_type(cell) == 5 {
                return Ok(args.get(1).copied().unwrap_or(Value::Void));
            }
            to_tjs(cx, cell)
        }
        #[tjs::method(name = "missing", resumable = true)]
        fn missing(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() < 3 {
                return Err(NativeError::Missing(2));
            }
            if value::to_integer(cx.heap(), args[0])? as i32 != 0 {
                return Ok(NativeStep::Return(Value::Int(0)));
            }
            let n = column_index(cx, &self.snapshot, args[1])?;
            if n == usize::MAX {
                return Ok(NativeStep::Return(Value::Int(0)));
            }
            let result = to_tjs(cx, self.snapshot.row.get(n))?;
            Ok(NativeStep::SetProperty {
                object: args[2],
                key: Value::Void,
                value: result,
                flags: Default::default(),
                continuation: Box::new(MissingDone),
            })
        }
        #[tjs::constant(name = "SQLITE_INTEGER")]
        const INTEGER: i64 = 1;
        #[tjs::constant(name = "SQLITE_FLOAT")]
        const FLOAT: i64 = 2;
        #[tjs::constant(name = "SQLITE_TEXT")]
        const TEXT: i64 = 3;
        #[tjs::constant(name = "SQLITE_BLOB")]
        const BLOB: i64 = 4;
        #[tjs::constant(name = "SQLITE_NULL")]
        const NULL: i64 = 5;
    }
}
struct MissingDone;
impl Trace for MissingDone {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for MissingDone {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Int(1)))
    }
}
fn column_index(cx: &NativeCx<'_>, snapshot: &Snapshot, column: Value) -> NativeResult<usize> {
    Ok(match column {
        Value::Int(_) | Value::Real(_) => {
            let n = value::to_integer(cx.heap(), column)? as i32;
            usize::try_from(n).unwrap_or(usize::MAX)
        }
        Value::Str(id) => {
            let name = String::from_utf16_lossy(tjs_core::string::c_string(cx.heap().string(id)?));
            snapshot
                .names
                .iter()
                .position(|s| s.eq_ignore_ascii_case(&name))
                .unwrap_or(usize::MAX)
        }
        _ => usize::MAX,
    })
}
fn cell_type(cell: Option<&Cell>) -> i64 {
    match cell {
        Some(Cell::Integer(_)) => 1,
        Some(Cell::Real(_)) => 2,
        Some(Cell::Text(_)) => 3,
        Some(Cell::Blob(_)) => 4,
        _ => 5,
    }
}
fn from_tjs(cx: &NativeCx<'_>, v: Value) -> NativeResult<Cell> {
    Ok(match v {
        Value::Int(n) => Cell::Integer(n),
        Value::Real(n) => Cell::Real(n),
        Value::Str(s) => Cell::Text(String::from_utf16_lossy(cx.heap().string(s)?)),
        Value::Octet(o) => Cell::Blob(cx.heap().octet(o)?.to_vec()),
        _ => Cell::Null,
    })
}
fn to_tjs(cx: &mut NativeCx<'_>, v: Option<&Cell>) -> NativeResult<Value> {
    Ok(match v {
        Some(Cell::Integer(n)) => Value::Int(*n),
        Some(Cell::Real(n)) => Value::Real(*n),
        Some(Cell::Text(s)) => Value::Str(
            cx.heap_mut().alloc_string(
                s.split('\0')
                    .next()
                    .unwrap_or("")
                    .encode_utf16()
                    .collect::<Vec<_>>(),
            ),
        ),
        Some(Cell::Blob(b)) => Value::Octet(cx.heap_mut().alloc_octet(b.clone())),
        _ => Value::Void,
    })
}
fn sql(cx: &NativeCx<'_>, v: Value) -> NativeResult<String> {
    if let Value::Str(s) = v {
        Ok(String::from_utf16_lossy(tjs_core::string::c_string(
            cx.heap().string(s)?,
        )))
    } else {
        Err(NativeError::Type("an SQL string"))
    }
}
fn position(cx: &NativeCx<'_>, v: Value) -> NativeResult<Position> {
    Ok(match v {
        Value::Int(_) | Value::Real(_) => {
            Position::Index((value::to_integer(cx.heap(), v)? as i32).wrapping_add(1) as usize)
        }
        Value::Str(s) => Position::Name(String::from_utf16_lossy(tjs_core::string::c_string(
            cx.heap().string(s)?,
        ))),
        _ => Position::Index(0),
    })
}
