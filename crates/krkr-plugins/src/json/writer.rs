use super::{ArrayApi, DEPTH, LIMIT, error};
use krkr_engine::{assets::local, storages};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::Write,
    rc::Rc,
};
use tjs_core::{
    Heap, HeapError, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep,
    NativeTryContinuation, ObjId, ObjRef, ObjectKind, Value, member, value,
};

#[derive(Default)]
struct Output {
    text: Vec<u16>,
    total: usize,
    indent: usize,
    lf: bool,
    sink: Option<Sink>,
}
struct Sink {
    file: File,
    vfs: storages::Shared,
    path: std::path::PathBuf,
    limit: usize,
    written: usize,
    utf8: bool,
}
impl Output {
    fn buffered(&mut self, text: &[u16]) -> NativeResult<()> {
        if text.len() > LIMIT.saturating_sub(self.total) {
            return Err(NativeError::Message("JSON exceeds output size limit"));
        }
        self.total += text.len();
        self.text.extend_from_slice(text);
        Ok(())
    }
    fn append(&mut self, text: &[u16]) -> NativeResult<()> {
        self.buffered(text)?;
        if self.sink.is_some() && self.text.len() >= 1024 {
            self.flush()?;
        }
        Ok(())
    }
    fn character(&mut self, unit: u16) -> NativeResult<()> {
        self.buffered(&[unit])
    }
    fn flush(&mut self) -> NativeResult<()> {
        let Some(sink) = &mut self.sink else {
            return Ok(());
        };
        let bytes = if sink.utf8 {
            // TVPWideCharToUtf8String in the same commit's
            // src/core/base/CharacterSet.cpp encodes each UTF-16 code unit.
            // Keep its CESU-8 result for surrogate pairs and lone surrogates.
            let mut bytes = Vec::new();
            for &unit in &self.text {
                match unit {
                    0..=0x7f => bytes.push(unit as u8),
                    0x80..=0x7ff => bytes.extend_from_slice(&[
                        (0xc0 | (unit >> 6)) as u8,
                        (0x80 | (unit & 0x3f)) as u8,
                    ]),
                    _ => bytes.extend_from_slice(&[
                        (0xe0 | (unit >> 12)) as u8,
                        (0x80 | ((unit >> 6) & 0x3f)) as u8,
                        (0x80 | (unit & 0x3f)) as u8,
                    ]),
                }
                if bytes.len() > sink.limit.saturating_sub(sink.written) {
                    return Err(NativeError::Message("JSON exceeds output size limit"));
                }
            }
            bytes
        } else {
            // CP_ACP uses the portable engine's modern default UTF-8.
            String::from_utf16(&self.text)
                .map_err(|_| NativeError::Message("isolated surrogate in JSON UTF-8 output"))?
                .into_bytes()
        };
        if bytes.len() > sink.limit.saturating_sub(sink.written) {
            return Err(NativeError::Message("JSON exceeds output size limit"));
        }
        let result = sink.file.write_all(&bytes);
        sink.vfs.borrow_mut().invalidate_file(&sink.path);
        result.map_err(error)?;
        sink.file.flush().map_err(error)?;
        sink.written += bytes.len();
        self.text.clear();
        Ok(())
    }
    fn ascii(&mut self, text: &str) -> NativeResult<()> {
        self.append(&text.bytes().map(u16::from).collect::<Vec<_>>())
    }
    fn newline(&mut self) -> NativeResult<()> {
        self.ascii(if self.lf { "\n" } else { "\r\n" })?;
        for _ in 0..self.indent {
            self.character(32)?;
        }
        Ok(())
    }
    fn open(&mut self, array: bool) -> NativeResult<()> {
        self.character(if array { 91 } else { 123 })?;
        self.indent += 1;
        self.newline()
    }
    fn close(&mut self, array: bool) -> NativeResult<()> {
        self.indent -= 1;
        self.newline()?;
        self.character(if array { 93 } else { 125 })
    }
    fn quote(&mut self, text: &[u16]) -> NativeResult<()> {
        self.character(34)?;
        for &unit in tjs_core::string::c_string(text) {
            match unit {
                34 => self.ascii("\\\"")?,
                92 => self.ascii("\\\\")?,
                8 => self.ascii("\\b")?,
                12 => self.ascii("\\f")?,
                10 => self.ascii("\\n")?,
                13 => self.ascii("\\r")?,
                9 => self.ascii("\\t")?,
                0..=31 => self.ascii(&format!("\\u{unit:04x}"))?,
                _ => self.character(unit)?,
            }
        }
        self.character(34)
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
}
#[derive(tjs_bind::Trace)]
enum Pending {
    Count,
    Item,
}
#[derive(tjs_bind::Trace)]
struct Serialize {
    #[trace(skip = "Output owns text and a file/VFS sink without TJS handles")]
    output: Rc<RefCell<Output>>,
    api: ArrayApi,
    stack: Vec<Frame>,
    value: Option<Value>,
    pending: Option<Pending>,
    missing: ObjId,
}
impl Serialize {
    fn count(&mut self, heap: &Heap, result: Value) -> NativeResult<()> {
        let count = value::to_integer(heap, result)? as i32;
        let Input::Array { count: total, .. } = &mut self.stack.last_mut().unwrap().input else {
            unreachable!("count is requested only for arrays")
        };
        *total = count;
        Ok(())
    }
    fn object(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        object: ObjId,
    ) -> NativeResult<NativeStep> {
        if self.stack.len() >= DEPTH || self.stack.iter().any(|f| f.object == object) {
            return Err(NativeError::Message("JSON structure is cyclic or too deep"));
        }
        // IsInstanceOf and EnumMembers return invalid-object statuses which the
        // original ignores: invalid objects serialize as an empty dictionary.
        let valid = cx.heap().is_valid(object)?;
        let name = Value::Str(cx.heap_mut().alloc_string([65, 114, 114, 97, 121]));
        let array = valid && value::instance_of(cx.heap_mut(), Value::Obj(object.into()), name)?;
        self.output.borrow_mut().open(array)?;
        let input = if array {
            Input::Array { index: 0, count: 0 }
        } else {
            let entries = if valid {
                cx.heap()
                    .all_members_with_flags(object)?
                    .map(|(key, _, _, _)| Ok(cx.heap().symbol(key)?.to_vec()))
                    .collect::<NativeResult<Vec<_>>>()?
            } else {
                Vec::new()
            };
            Input::Members(entries.into_iter())
        };
        self.stack.push(Frame {
            object,
            input,
            first: true,
        });
        if array {
            let Value::Obj(mut count) = self.api.0 else {
                unreachable!("checked at link")
            };
            // AsObject discards the captured closure's context.
            count.this = Some(object);
            match member::get(cx.heap_mut(), Value::Obj(count), Value::Void) {
                Ok(value) => self.count(cx.heap(), value)?,
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
                Err(error) => return Err(NativeError::Detail(error.to_string())),
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
impl NativeContinuation for Serialize {
    fn resume(
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
        // Bound native work. Every pending object/value remains rooted while
        // count getters, inherited-array missing handlers, and GC run.
        for _ in 0..64 {
            if let Some(value) = self.value.take() {
                match value {
                    Value::Obj(ObjRef {
                        object: Some(object),
                        ..
                    }) => return self.object(cx, object),
                    Value::Str(id) => self.output.borrow_mut().quote(cx.heap().string(id)?)?,
                    Value::Int(_) => {
                        let text = value::to_string_units(cx.heap(), value)?;
                        self.output.borrow_mut().buffered(&text)?;
                    }
                    Value::Real(_) => {
                        let text = value::to_string_units(cx.heap(), value)?;
                        self.output
                            .borrow_mut()
                            .append(text.strip_prefix(&[43]).unwrap_or(&text))?;
                    }
                    _ => self.output.borrow_mut().ascii("null")?,
                }
            }
            let Some(frame) = self.stack.last_mut() else {
                return Ok(NativeStep::Return(Value::Void));
            };
            let array = matches!(frame.input, Input::Array { .. });
            let (key, value, index) = match &mut frame.input {
                Input::Array { index, count } if *index < *count => {
                    let current = *index;
                    *index += 1;
                    (None, None, Some(current))
                }
                Input::Members(entries) => match entries.next() {
                    Some(key) => {
                        // Freeze enumeration names, not values: an earlier
                        // nested count getter can replace a later raw slot.
                        // Like upstream, insertion/deletion during enumeration
                        // has no specified order; removed names are skipped.
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
                        self.output.borrow_mut().close(false)?;
                        continue;
                    }
                },
                _ => {
                    self.stack.pop();
                    self.output.borrow_mut().close(array)?;
                    continue;
                }
            };
            if !frame.first {
                self.output.borrow_mut().character(44)?;
                self.output.borrow_mut().newline()?;
            }
            frame.first = false;
            if let Some(key) = key {
                self.output.borrow_mut().quote(&key)?;
                self.output.borrow_mut().character(58)?;
            }
            self.value = value;
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
                        key: Value::Int(i64::from(index)),
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
#[derive(tjs_bind::Trace)]
struct Finish {
    #[trace(skip = "Output owns text and a file/VFS sink without TJS handles")]
    output: Rc<RefCell<Output>>,
}
impl NativeTryContinuation for Finish {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        let saved = self.output.borrow().sink.is_some();
        self.output.borrow_mut().flush()?;
        let text = std::mem::take(&mut self.output.borrow_mut().text);
        Ok(match result {
            Err(error) => NativeStep::Throw(error),
            Ok(_) if saved => NativeStep::Return(Value::Void),
            Ok(_) => NativeStep::Return(Value::Str(cx.heap_mut().alloc_string(text))),
        })
    }
}
pub(super) fn serialize(
    cx: &mut NativeCx<'_>,
    input: Value,
    newline: i32,
    path: Option<Vec<u16>>,
    utf8: bool,
) -> NativeResult<NativeStep> {
    let scripts = cx
        .heap()
        .registered_class("Scripts")
        .expect("installed Scripts");
    let api = cx
        .heap_mut()
        .with_native_state::<ArrayApi, _>(scripts, |api| api.clone())?;
    let sink = if let Some(path) = path {
        let vfs = storages::service(cx)?;
        let full = vfs.borrow().full_path(&path).map_err(error)?;
        let path = local::resolve(&local::from_storage(&full).map_err(error)?).map_err(error)?;
        let limit = vfs.borrow().limits().max_read_bytes.min(LIMIT);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(error)?;
        vfs.borrow_mut().invalidate_file(&path);
        Some(Sink {
            file,
            vfs,
            path,
            limit,
            written: 0,
            utf8,
        })
    } else {
        None
    };
    let output = Rc::new(RefCell::new(Output {
        lf: newline == 1,
        sink,
        ..Default::default()
    }));
    let missing = cx.heap_mut().alloc_dictionary();
    Ok(NativeStep::Try {
        task: Box::new(Serialize {
            output: output.clone(),
            api,
            stack: vec![],
            value: Some(input),
            pending: None,
            missing,
        }),
        continuation: Box::new(Finish { output }),
    })
}
