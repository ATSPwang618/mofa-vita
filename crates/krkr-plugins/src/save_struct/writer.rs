use super::Root;
use krkr_engine::{assets::local, storages};
use std::{cell::RefCell, fs::File, io::Write, rc::Rc};
use tjs_core::{
    Heap, HeapError, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId,
    ObjRef, ObjectKind, Value, member, value,
};

const BYTES: usize = 16 * 1024 * 1024;
const ITEMS: usize = 262144;
const DEPTH: usize = 128;
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
struct Sink {
    file: File,
    vfs: storages::Shared,
    path: std::path::PathBuf,
}
struct Output {
    text: Vec<u16>,
    total: usize,
    limit: usize,
    lf: bool,
    sink: Option<Sink>,
}
impl Output {
    fn append(&mut self, units: &[u16]) -> NativeResult<()> {
        if units.len() > self.limit.saturating_sub(self.total) {
            return Err(NativeError::Message("saveStruct exceeds output size limit"));
        }
        self.total += units.len();
        self.text.extend_from_slice(units);
        Ok(())
    }
    fn ascii(&mut self, text: &str) -> NativeResult<()> {
        self.append(&text.bytes().map(u16::from).collect::<Vec<_>>())
    }
    fn newline(&mut self) -> NativeResult<()> {
        self.ascii(if self.lf { "\n" } else { "\r\n" })
    }
    fn flush(&mut self) -> NativeResult<()> {
        if let Some(sink) = &mut self.sink
            && !self.text.is_empty()
        {
            let bytes = self
                .text
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect::<Vec<_>>();
            let result = sink.file.write_all(&bytes);
            sink.vfs.borrow_mut().invalidate_file(&sink.path);
            result.map_err(error)?;
            self.text.clear();
        }
        Ok(())
    }
}
#[derive(tjs_bind::Trace)]
enum Input {
    Array { index: i32, count: i32 },
    Members(std::vec::IntoIter<Vec<u16>>),
}
#[derive(tjs_bind::Trace)]
struct Frame {
    object: ObjId,
    input: Input,
    first: bool,
    lines: bool,
}
#[derive(tjs_bind::Trace)]
enum Pending {
    Count,
    Item,
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum ScalarKind {
    Quoted,
    Octet,
    Line,
}
#[derive(tjs_bind::Trace)]
struct Scalar {
    value: Value,
    position: usize,
    kind: ScalarKind,
    suffix: &'static str,
}
impl Scalar {
    fn advance(&mut self, heap: &Heap, output: &mut Output) -> NativeResult<bool> {
        match self.value {
            Value::Str(id) => {
                let text = heap.string(id)?;
                let end = text.len().min(self.position.saturating_add(4096));
                while self.position < end {
                    let unit = text[self.position];
                    if unit == 0 {
                        self.position = text.len();
                        break;
                    }
                    if matches!(self.kind, ScalarKind::Quoted) && matches!(unit, 34 | 92) {
                        output.append(&[92])?;
                    }
                    output.append(&[unit])?;
                    self.position += 1;
                }
                if self.position < text.len() {
                    return Ok(false);
                }
            }
            Value::Octet(id) => {
                let data = heap.octet(id)?;
                let end = data.len().min(self.position.saturating_add(4096));
                const HEX: &[u8; 16] = b"0123456789abcdef";
                for &byte in &data[self.position..end] {
                    output.append(&[
                        HEX[(byte >> 4) as usize].into(),
                        HEX[(byte & 15) as usize].into(),
                        32,
                    ])?;
                }
                self.position = end;
                if end < data.len() {
                    return Ok(false);
                }
            }
            _ => unreachable!("validated scalar"),
        }
        match self.kind {
            ScalarKind::Quoted => output.append(&[34])?,
            ScalarKind::Octet => output.ascii("%>")?,
            ScalarKind::Line => output.newline()?,
        }
        output.ascii(self.suffix)?;
        Ok(true)
    }
}
#[derive(tjs_bind::Trace)]
struct Serialize {
    #[trace(skip = "Output owns text and a file/VFS sink without TJS handles")]
    output: Rc<RefCell<Output>>,
    api: Value,
    root: Option<(ObjId, Root)>,
    stack: Vec<Frame>,
    value: Option<Value>,
    scalar: Option<Scalar>,
    pending: Option<Pending>,
    missing: ObjId,
    names_left: usize,
    name_units_left: usize,
}
impl Serialize {
    fn count(&mut self, heap: &Heap, result: Value) -> NativeResult<()> {
        let count = value::to_integer(heap, result)? as i32;
        if count > ITEMS as i32 {
            return Err(NativeError::Message("saveStruct array exceeds item limit"));
        }
        let Input::Array { count: total, .. } = &mut self.stack.last_mut().unwrap().input else {
            unreachable!("array count");
        };
        *total = count;
        Ok(())
    }
    fn scalar(&mut self, value: Value, kind: ScalarKind, suffix: &'static str) -> NativeResult<()> {
        self.output.borrow_mut().ascii(match kind {
            ScalarKind::Quoted => "\"",
            ScalarKind::Octet => "<% ",
            ScalarKind::Line => "",
        })?;
        self.scalar = Some(Scalar {
            value,
            position: 0,
            kind,
            suffix,
        });
        Ok(())
    }
    fn object(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        object: ObjId,
        forced: Option<Root>,
    ) -> NativeResult<NativeStep> {
        if self.stack.len() >= DEPTH || self.stack.iter().any(|f| f.object == object) {
            return Err(NativeError::Message(
                "saveStruct structure is cyclic or too deep",
            ));
        }
        let valid = cx.heap().is_valid(object)?;
        let array = match forced {
            Some(Root::Array | Root::Lines) => true,
            Some(Root::Dictionary) => false,
            None => {
                let name = Value::Str(cx.heap_mut().alloc_string([65, 114, 114, 97, 121]));
                valid && value::instance_of(cx.heap_mut(), Value::Obj(object.into()), name)?
            }
        };
        let lines = matches!(forced, Some(Root::Lines));
        if !lines {
            self.output
                .borrow_mut()
                .ascii(if array { "[" } else { "%[" })?;
        }
        let input = if array {
            Input::Array { index: 0, count: 0 }
        } else {
            let mut names = Vec::new();
            if valid {
                for (key, _, hidden, _) in cx.heap().all_members_with_flags(object)? {
                    if hidden {
                        continue;
                    }
                    let name = cx.heap().symbol(key)?;
                    // One budget across the whole traversal, so nested
                    // dictionary snapshots cannot each reserve the full limit.
                    if self.names_left == 0 || name.len() > self.name_units_left {
                        return Err(NativeError::Message(
                            "saveStruct dictionary exceeds item limit",
                        ));
                    }
                    self.names_left -= 1;
                    self.name_units_left -= name.len();
                    names.push(name.to_vec());
                }
            }
            Input::Members(names.into_iter())
        };
        self.stack.push(Frame {
            object,
            input,
            first: true,
            lines,
        });
        if array {
            let Value::Obj(mut count) = self.api else {
                unreachable!("validated at link")
            };
            count.this = Some(object);
            // Publish '[' even before a native count getter. Script getters
            // resume on this same VM; no hidden runtime or blocking execution.
            self.output.borrow_mut().flush()?;
            match member::get(cx.heap_mut(), Value::Obj(count), Value::Void) {
                Ok(result) => self.count(cx.heap(), result)?,
                Err(member::MemberError::Invoke {
                    function,
                    argument: None,
                }) => {
                    self.pending = Some(Pending::Count);
                    return Ok(NativeStep::Call {
                        function,
                        arguments: vec![],
                        continuation: self,
                    });
                }
                Err(
                    member::MemberError::NotProperty
                    | member::MemberError::AccessDenied
                    | member::MemberError::Heap(HeapError::InvalidObject)
                    | member::MemberError::Native(NativeError::Heap(
                        HeapError::NotArray | HeapError::InvalidObject,
                    ))
                    | member::MemberError::Native(NativeError::This),
                ) => {}
                Err(e) => return Err(error(e)),
            }
        }
        Ok(NativeStep::Continue(self))
    }
    fn advance(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.pending.take() {
            Some(Pending::Count) => self.count(cx.heap(), result)?,
            Some(Pending::Item) if !matches!(result, Value::Obj(ObjRef { object: Some(id), .. }) if id == self.missing) => {
                self.value = Some(result)
            }
            _ => {}
        }
        if let Some((object, root)) = self.root.take() {
            return self.object(cx, object, Some(root));
        }
        for _ in 0..64 {
            if let Some(scalar) = &mut self.scalar {
                if !scalar.advance(cx.heap(), &mut self.output.borrow_mut())? {
                    return Ok(NativeStep::Continue(self));
                }
                self.scalar = None;
            }
            if let Some(value) = self.value.take() {
                if self.stack.last().is_some_and(|f| f.lines) {
                    if !matches!(value, Value::Str(_)) {
                        return Err(NativeError::Type("a String array item for save2"));
                    }
                    self.scalar(value, ScalarKind::Line, "")?;
                    continue;
                }
                match value {
                    Value::Obj(ObjRef {
                        object: Some(object),
                        ..
                    }) => return self.object(cx, object, None),
                    Value::Str(_) => {
                        self.scalar(value, ScalarKind::Quoted, "")?;
                        continue;
                    }
                    Value::Octet(_) => {
                        self.scalar(value, ScalarKind::Octet, "")?;
                        continue;
                    }
                    Value::Int(_) | Value::Real(_) => {
                        let text = value::to_string_units(cx.heap(), value)?;
                        let mut out = self.output.borrow_mut();
                        out.ascii(if matches!(value, Value::Int(_)) {
                            "int "
                        } else {
                            "real "
                        })?;
                        out.append(&text)?;
                    }
                    Value::Void => self.output.borrow_mut().ascii("void")?,
                    // krkr2 accidentally emits the C++ token nullptr. Keep a
                    // valid TJS null literal rather than an undefined identifier.
                    _ => self.output.borrow_mut().ascii("null")?,
                }
            }
            let Some(frame) = self.stack.last_mut() else {
                let mut out = self.output.borrow_mut();
                return Ok(NativeStep::Return(if out.sink.is_some() {
                    Value::Void
                } else {
                    Value::Str(cx.heap_mut().alloc_string(std::mem::take(&mut out.text)))
                }));
            };
            let array = matches!(frame.input, Input::Array { .. });
            let (key, item, index) = match &mut frame.input {
                Input::Array { index, count } if *index < *count => {
                    let current = *index;
                    *index += 1;
                    (None, None, Some(current))
                }
                Input::Members(entries) => match entries.next() {
                    Some(key) => {
                        // Snapshot names, read current raw slots. Nested count
                        // callbacks can change a later value; never run getters.
                        if !cx.heap().is_valid(frame.object)? {
                            continue;
                        }
                        let name = cx.heap_mut().intern(&key);
                        match cx.heap().member_with_flags(frame.object, name)? {
                            Some((value, false, _)) => (Some(key), Some(value), None),
                            _ => continue,
                        }
                    }
                    None => {
                        self.stack.pop();
                        self.output.borrow_mut().ascii("]")?;
                        continue;
                    }
                },
                _ => {
                    let lines = frame.lines;
                    self.stack.pop();
                    if !lines {
                        self.output.borrow_mut().ascii("]")?;
                    }
                    continue;
                }
            };
            if !frame.first && !frame.lines {
                self.output.borrow_mut().ascii(",")?;
                if !array {
                    self.output.borrow_mut().newline()?;
                }
            }
            frame.first = false;
            self.value = item;
            if let Some(key) = key {
                let key = Value::Str(cx.heap_mut().alloc_string(key));
                self.scalar(key, ScalarKind::Quoted, "=>")?;
                continue;
            }
            if let Some(index) = index {
                let object = frame.object;
                if !cx.heap().is_valid(object)? {
                    continue;
                }
                if cx.heap().object(object)?.kind() == ObjectKind::Array {
                    self.value = Some(
                        cx.heap()
                            .array(object)?
                            .get(index as usize)
                            .copied()
                            .unwrap_or(Value::Void),
                    );
                } else {
                    self.pending = Some(Pending::Item);
                    return Ok(NativeStep::GetOr {
                        object: Value::Obj(ObjRef::bound(object)),
                        key: Value::Int(index.into()),
                        raw: true,
                        fallback: Value::Obj(self.missing.into()),
                        continuation: self,
                    });
                }
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
impl NativeContinuation for Serialize {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep> {
        let output = self.output.clone();
        let step = self.advance(cx, result);
        // Flush accepted prefixes on success, error, callback and budget yield.
        // Cancellation drops only an empty buffer and closes the owned file.
        output.borrow_mut().flush()?;
        step
    }
}
pub(super) fn start(
    cx: &mut NativeCx<'_>,
    root: Root,
    api: Value,
    path: Option<Value>,
    newline: Option<Value>,
) -> NativeResult<NativeStep> {
    let mut limit = BYTES / 2;
    let sink = if let Some(path) = path {
        if matches!(path, Value::Octet(_)) {
            return Err(NativeError::Type("a string-convertible filename"));
        }
        let path = value::to_string_units(cx.heap(), path)?;
        let vfs = storages::service(cx)?;
        let full = vfs.borrow().full_path(&path).map_err(error)?;
        let local = local::resolve(&local::from_storage(&full).map_err(error)?).map_err(error)?;
        limit = limit.min(vfs.borrow().limits().max_read_bytes / 2);
        let file = File::create(&local).map_err(error)?;
        vfs.borrow_mut().invalidate_file(&local);
        Some(Sink {
            file,
            vfs,
            path: local,
        })
    } else {
        None
    };
    // Open/truncate precedes this conversion in the reference. utf is ignored.
    let lf = newline
        .map(|v| value::to_integer(cx.heap(), v))
        .transpose()?
        .unwrap_or(0) as i32
        != 0;
    let output = Rc::new(RefCell::new(Output {
        text: vec![],
        total: 0,
        limit,
        lf,
        sink,
    }));
    let owner = cx.this();
    let missing = cx.heap_mut().alloc_dictionary();
    Ok(NativeStep::Continue(Box::new(Serialize {
        output,
        api,
        root: Some((owner, root)),
        stack: vec![],
        value: None,
        scalar: None,
        pending: None,
        missing,
        names_left: ITEMS,
        name_units_left: BYTES / 2,
    })))
}
