//! Each connection and all its live cursors stay on one owned worker.
//! No script values or raw SQLite handles cross the boundary.
pub(super) use rusqlite::types::Value as Cell;
use rusqlite::{Connection, Rows, Statement, types::Value as SqlValue};
use self_cell::{MutBorrow, self_cell};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};
use tjs_core::{NativeError, NativeResult};
type RowsRef<'a> = Rows<'a>;
self_cell!(
    struct Cursor<'conn> {
        owner: MutBorrow<Statement<'conn>>,
        #[covariant]
        dependent: RowsRef,
    }
);
struct Prepared<'conn> {
    statement: Option<Statement<'conn>>,
    cursor: Option<Cursor<'conn>>,
    sql: String,
    names: Vec<String>,
    row: Vec<SqlValue>,
    bind_pos: usize,
    last_code: i32,
}
#[derive(Clone, Default)]
pub(super) struct Status {
    pub code: i32,
    pub message: String,
    pub rowid: i64,
}
#[derive(Clone, Default)]
pub(super) struct Snapshot {
    pub sql: String,
    pub names: Vec<String>,
    pub row: Vec<Cell>,
}
pub(super) struct Reply {
    pub code: i32,
    pub snapshot: Option<Snapshot>,
}
pub(super) enum Input {
    Failed(String),
    Local(std::path::PathBuf),
    Special(String),
    Archive(krkr_engine::assets::ReadPlan),
}
pub(super) enum Command {
    Prepare(u64, String),
    Close(u64),
    Bind(u64, Vec<(Position, Cell)>),
    BindAt(u64, Cell, Option<Position>),
    Step(u64),
    Reset(u64),
    Transaction(&'static str),
}
#[derive(Clone)]
pub(super) enum Position {
    Index(usize),
    Name(String),
}
struct Message {
    command: Command,
    reply: mpsc::SyncSender<Reply>,
}
pub(super) struct Database {
    sender: mpsc::SyncSender<Message>,
    cancel: Arc<AtomicBool>,
    pub status: Arc<Mutex<Status>>,
    next: AtomicU64,
}
impl Drop for Database {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Database {
    pub fn statement_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
    pub fn close(&self, id: u64) {
        let (tx, _) = mpsc::sync_channel(1);
        let _ = self.sender.try_send(Message {
            command: Command::Close(id),
            reply: tx,
        });
    }
    pub fn request(&self, command: Command, stop: &AtomicBool) -> NativeResult<Reply> {
        if stop.load(Ordering::Acquire) {
            return Err(NativeError::Message("SQLite operation cancelled"));
        }
        self.cancel.store(false, Ordering::Release);
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .try_send(Message { command, reply: tx })
            .map_err(|_| NativeError::Message("SQLite worker queue is unavailable"))?;
        loop {
            if stop.load(Ordering::Acquire) {
                self.cancel.store(true, Ordering::Release);
                return Err(NativeError::Message("SQLite operation cancelled"));
            }
            match krkr_engine::protocol::channel::recv_timeout(&rx, Duration::from_millis(10)) {
                Ok(r) => return Ok(r),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(NativeError::Message("SQLite worker stopped")),
            }
        }
    }
    pub fn open(input: Input, stop: &AtomicBool) -> NativeResult<Arc<Self>> {
        let (tx, rx) = mpsc::sync_channel::<Message>(128);
        let cancel = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status {
            code: 0,
            message: "not an error".into(),
            rowid: 0,
        }));
        let db = Arc::new(Self {
            sender: tx,
            cancel: cancel.clone(),
            status: status.clone(),
            next: AtomicU64::new(1),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("krkr-sqlite".into())
            .spawn(move || {
                let opened = open(input, &cancel);
                if let Err(e) = &opened {
                    record_error(&status, e);
                }
                let _ = ready_tx.send(());
                let Ok(connection) = opened else {
                    while let Ok(message) = rx.recv() {
                        let _ = message.reply.send(Reply {
                            code: status.lock().unwrap().code,
                            snapshot: None,
                        });
                    }
                    return;
                };
                let mut statements = BTreeMap::new();
                while let Ok(message) = rx.recv() {
                    let closing = matches!(&message.command, Command::Close(_));
                    let (result, snapshot) = execute(&connection, &mut statements, message.command);
                    let code = match result {
                        Ok(code) if closing => code,
                        Ok(code) => {
                            let mut s = status.lock().unwrap();
                            s.code = if code == 100 { 100 } else { 0 };
                            s.message = if code == 100 {
                                "another row available"
                            } else {
                                "not an error"
                            }
                            .into();
                            code
                        }
                        Err(e) => {
                            record_error(&status, &e);
                            status.lock().unwrap().code
                        }
                    };
                    status.lock().unwrap().rowid = connection.last_insert_rowid();
                    let _ = message.reply.send(Reply { code, snapshot });
                }
            })
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        loop {
            if stop.load(Ordering::Acquire) {
                db.cancel.store(true, Ordering::Release);
                return Err(NativeError::Message("SQLite open cancelled"));
            }
            match krkr_engine::protocol::channel::recv_timeout(&ready_rx, Duration::from_millis(10))
            {
                Ok(()) => return Ok(db),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(NativeError::Message("SQLite open worker stopped")),
            }
        }
    }
}
fn record_error(status: &Mutex<Status>, e: &rusqlite::Error) {
    let mut s = status.lock().unwrap();
    s.code = e.sqlite_error().map_or(1, |e| e.extended_code & 255);
    s.message = e.to_string();
}
fn open(input: Input, cancel: &Arc<AtomicBool>) -> rusqlite::Result<Connection> {
    let connection = match input {
        Input::Failed(message) => {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(14),
                Some(message),
            ));
        }
        Input::Local(path) => Connection::open(path)?,
        Input::Special(name) => Connection::open(name)?,
        Input::Archive(plan) => {
            let size = usize::try_from(plan.bytes)
                .ok()
                .filter(|&n| n <= 64 * 1024 * 1024)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            let bytes = plan
                .read_interruptible(0, || cancel.load(Ordering::Acquire))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let mut c = Connection::open_in_memory()?;
            c.deserialize_read_exact(rusqlite::MAIN_DB, &bytes[..], size, true)?;
            c
        }
    };
    use rusqlite::limits::Limit;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, 8 * 1024 * 1024)?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 1024 * 1024)?;
    connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, 1024)?;
    connection.busy_timeout(Duration::ZERO)?;
    connection.execute_batch("PRAGMA cache_size=-2048;")?;
    super::normalize::install(&connection)?;
    let interrupt = cancel.clone();
    connection.progress_handler(1000, Some(move || interrupt.load(Ordering::Acquire)))?;
    Ok(connection)
}
impl<'c> Prepared<'c> {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sql: self.sql.clone(),
            names: self.names.clone(),
            row: self.row.clone(),
        }
    }
    fn reset(&mut self) -> i32 {
        if let Some(cursor) = self.cursor.take() {
            self.statement = Some(cursor.into_owner().into_inner());
        }
        self.row.clear();
        self.bind_pos = 1;
        let code = if self.last_code == 100 || self.last_code == 101 {
            0
        } else {
            self.last_code
        };
        self.last_code = 0;
        code
    }
    fn position(&self, p: &Position) -> rusqlite::Result<usize> {
        match p {
            Position::Index(n) => Ok(*n),
            Position::Name(name) => Ok(self
                .statement
                .as_ref()
                .ok_or(rusqlite::Error::InvalidQuery)?
                .parameter_index(name)?
                .unwrap_or(0)),
        }
    }
    fn bind(&mut self, p: &Position, value: &Cell) -> rusqlite::Result<()> {
        let index = self.position(p)?;
        self.statement
            .as_mut()
            .ok_or(rusqlite::Error::InvalidQuery)?
            .raw_bind_parameter(index, value)
    }
    fn step(&mut self) -> rusqlite::Result<i32> {
        if self.cursor.is_none() {
            let stmt = self.statement.take().ok_or(rusqlite::Error::InvalidQuery)?;
            self.cursor = Some(Cursor::new(MutBorrow::new(stmt), |s| {
                s.borrow_mut().raw_query()
            }));
        }
        let result = self.cursor.as_mut().unwrap().with_dependent_mut(
            |_, rows| -> rusqlite::Result<Option<Vec<Cell>>> {
                let Some(row) = rows.next()? else {
                    return Ok(None);
                };
                let count = row.as_ref().column_count();
                let mut values = Vec::with_capacity(count);
                let mut bytes = 0;
                for i in 0..count {
                    let v: Cell = row.get(i)?;
                    bytes += match &v {
                        Cell::Text(s) => s.len(),
                        Cell::Blob(s) => s.len(),
                        _ => 8,
                    };
                    if bytes > 8 * 1024 * 1024 {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    values.push(v);
                }
                Ok(Some(values))
            },
        );
        match result {
            Ok(Some(row)) => {
                self.row = row;
                self.last_code = 100;
                Ok(100)
            }
            Ok(None) => {
                self.last_code = 101;
                self.reset();
                Ok(101)
            }
            Err(e) => {
                self.last_code = e.sqlite_error().map_or(1, |e| e.extended_code & 255);
                self.reset();
                Err(e)
            }
        }
    }
}
fn execute<'c>(
    db: &'c Connection,
    statements: &mut BTreeMap<u64, Prepared<'c>>,
    command: Command,
) -> (rusqlite::Result<i32>, Option<Snapshot>) {
    let mut snapshot = None;
    let result = (|| -> rusqlite::Result<i32> {
        match command {
            Command::Prepare(id, sql) => {
                statements.remove(&id);
                if statements.len() >= 128 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                // SQLite itself decides whether a prefix is complete (notably
                // CREATE TRIGGER bodies contain internal semicolons).
                let mut end = first_statement(&sql).len();
                let statement = loop {
                    match db.prepare(&sql[..end]) {
                        Ok(statement) => break statement,
                        Err(_) if end < sql.len() => end += first_statement(&sql[end..]).len(),
                        Err(error) => return Err(error),
                    }
                };
                let sql = sql[..end].to_owned();
                let names = statement
                    .column_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                let prepared = Prepared {
                    statement: Some(statement),
                    cursor: None,
                    sql,
                    names,
                    row: Vec::new(),
                    bind_pos: 1,
                    last_code: 0,
                };
                snapshot = Some(prepared.snapshot());
                statements.insert(id, prepared);
                Ok(0)
            }
            Command::Close(id) => {
                statements.remove(&id);
                Ok(0)
            }
            Command::Transaction(sql) => db.execute_batch(sql).map(|_| 0),
            command => {
                let id = match &command {
                    Command::Bind(id, ..)
                    | Command::BindAt(id, ..)
                    | Command::Step(id)
                    | Command::Reset(id) => *id,
                    _ => unreachable!(),
                };
                let s = statements
                    .get_mut(&id)
                    .ok_or(rusqlite::Error::InvalidQuery)?;
                let result = match command {
                    Command::Bind(_, values) => {
                        let mut result = Ok(0);
                        for (p, v) in values {
                            if let Err(e) = s.bind(&p, &v) {
                                result = Err(e);
                                break;
                            }
                        }
                        result
                    }
                    Command::BindAt(_, value, pos) => {
                        let p = match pos {
                            Some(p) => {
                                s.bind_pos = s.position(&p)?;
                                p
                            }
                            None => Position::Index(s.bind_pos),
                        };
                        let result = s.bind(&p, &value).map(|_| 0);
                        s.bind_pos = s.bind_pos.saturating_add(1);
                        result
                    }
                    Command::Step(_) => s.step(),
                    Command::Reset(_) => Ok(s.reset()),
                    _ => unreachable!(),
                };
                snapshot = Some(s.snapshot());
                result
            }
        }
    })();
    (result, snapshot)
}
fn first_statement(sql: &str) -> &str {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut quote = 0;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == quote {
                if b.get(i + 1) == Some(&quote) {
                    i += 1;
                } else {
                    quote = 0;
                }
            }
        } else if matches!(c, b'\'' | b'"' | b'`') {
            quote = c;
        } else if c == b'[' {
            quote = b']';
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && &b[i..i + 2] != b"*/" {
                i += 1;
            }
            i += 1;
        } else if c == b';' {
            return &sql[..i + 1];
        }
        i += 1;
    }
    sql
}
