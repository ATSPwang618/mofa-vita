//! kwidgets e7bed32 table.cpp and ncbind.hpp. Searches retain the source's
//! callback order, including its incremental (not prefix-only) text measurements.
use crate::exports::{Exports, arg, object};
use krkr_engine::plugins::Context;
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef,
    Value, value,
};

pub(crate) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    exports.function(
        cx,
        cx.global,
        "table_find_list_range",
        NativeCallable::Resumable(list_range),
    )?;
    exports.function(
        cx,
        cx.global,
        "table_find_text_range",
        NativeCallable::Resumable(text_range),
    )
}

const RESULT_LIMIT: usize = 262_144;
const TEXT_LIMIT: usize = 1_048_576;

fn integer(cx: &NativeCx<'_>, value: Value) -> NativeResult<i32> {
    Ok(value::to_integer(cx.heap(), value)? as i32)
}

// ncbPropAccessor takes AsObject(), not AsObjectClosure(): an input's bound this
// does not replace the receiver. Null dereferences in the C++ helper are rejected
// safely; invalid objects instead return a failed dispatch status.
fn receiver(value: Value) -> NativeResult<Value> {
    Ok(Value::Obj(ObjRef::bound(object(value)?)))
}

fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}

#[derive(Clone, Copy, tjs_bind::Trace)]
enum ListPhase {
    Count,
    Item { scan: bool },
    Position { scan: bool },
    Size,
}

#[derive(tjs_bind::Trace)]
struct ListRange {
    list: Value,
    count: i32,
    begin: i32,
    end: i32,
    index: i32,
    pos: i32,
    size: i32,
    phase: ListPhase,
    item: Value,
    item_pos: i32,
    result: Value,
}
impl ListRange {
    fn done(&self) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(self.result))
    }

    fn read(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        source: Value,
        key: Value,
    ) -> NativeResult<NativeStep> {
        if !cx.heap().is_valid(object(source)?)? {
            // Every GetValue/GetArrayCount constructs a fresh void temporary.
            // Failed statuses do not retain the preceding read's value.
            return Ok(NativeStep::Continue(self));
        }
        Ok(NativeStep::GetOr {
            object: source,
            key,
            raw: false,
            fallback: Value::Void,
            continuation: self,
        })
    }

    fn read_item(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        scan: bool,
    ) -> NativeResult<NativeStep> {
        self.phase = ListPhase::Item { scan };
        let (list, index) = (self.list, self.index);
        self.read(cx, list, Value::Int(i64::from(index)))
    }

    fn read_field(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        phase: ListPhase,
        name: &str,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        let item = receiver(self.item)?;
        let key = key(cx, name);
        // table.cpp's literal 8 is flags, not a default value. It is not
        // TJS_IGNOREPROP (0x800), so ordinary property getters still execute.
        self.read(cx, item, key)
    }

    fn search(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.begin >= self.end {
            return self.done();
        }
        // Avoid native signed overflow in begin + end for large array-like
        // counts; the source has no defined result for that overflow.
        self.index = self.begin + (self.end - self.begin) / 2;
        self.read_item(cx, false)
    }

    fn append_and_scan(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let result = object(self.result)?;
        if cx.heap().array(result)?.len() >= RESULT_LIMIT {
            return Err(NativeError::Message("table result exceeds size limit"));
        }
        // TJSCreateArrayObject uses its own private Array class, and the result
        // has not escaped. Its builtin add cannot invoke script callbacks.
        cx.heap_mut().array_push(result, self.item)?;
        self.index += 1;
        if self.index >= self.count {
            return self.done();
        }
        self.read_item(cx, true)
    }
}
impl NativeContinuation for ListRange {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.phase {
            ListPhase::Count => {
                self.count = integer(cx, result)?;
                self.end = self.count;
                self.result = Value::Obj(ObjRef::bound(cx.heap_mut().alloc_array()));
                if self.pos < 0 {
                    self.size = self.size.wrapping_add(self.pos);
                    self.pos = 0;
                }
                if self.size < 0 {
                    return self.done();
                }
                self.search(cx)
            }
            ListPhase::Item { scan } => {
                self.item = result;
                self.read_field(cx, ListPhase::Position { scan }, "pos")
            }
            ListPhase::Position { scan } => {
                self.item_pos = integer(cx, result)?;
                if scan {
                    if self.item_pos >= self.pos.wrapping_add(self.size) {
                        return self.done();
                    }
                    self.append_and_scan(cx)
                } else {
                    self.read_field(cx, ListPhase::Size, "size")
                }
            }
            ListPhase::Size => {
                let size = integer(cx, result)?;
                if self.item_pos <= self.pos && self.pos < self.item_pos.wrapping_add(size) {
                    return self.append_and_scan(cx);
                }
                if self.pos < self.item_pos {
                    self.end = self.index;
                } else {
                    self.begin = self.index + 1;
                }
                self.search(cx)
            }
        }
    }
}
fn list_range(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    // ncbind checks the complete arity before conversions or function-body work.
    arg(args, 2)?;
    let pos = integer(cx, args[1])?;
    let size = integer(cx, args[2])?;
    let list = receiver(args[0])?;
    let count = key(cx, "count");
    Box::new(ListRange {
        list,
        count: 0,
        begin: 0,
        end: 0,
        index: 0,
        pos,
        size,
        phase: ListPhase::Count,
        item: Value::Void,
        item_pos: 0,
        result: Value::Void,
    })
    .read(cx, list, count)
}

#[derive(Clone, Copy, tjs_bind::Trace)]
enum TextPhase {
    Full,
    Initial,
    Add,
    Subtract,
    Tail,
}

#[derive(tjs_bind::Trace)]
struct TextRange {
    font: Value,
    text: Vec<u16>,
    width: i32,
    omit: i32,
    begin: usize,
    end: usize,
    mid: usize,
    total: i32,
    index: usize,
    phase: TextPhase,
}
impl TextRange {
    fn measure(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        begin: usize,
        end: usize,
        phase: TextPhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        let mut units = &self.text[begin..end];
        if !matches!(phase, TextPhase::Full) {
            // substr_ttstr constructs ttstr(pointer, length), which stops at
            // NUL. The original full-string measurement keeps all UTF-16 units.
            if let Some(nul) = units.iter().position(|&unit| unit == 0) {
                units = &units[..nul];
            }
        }
        let text = Value::Str(cx.heap_mut().alloc_string(units));
        let key = key(cx, "getTextWidth");
        Ok(NativeStep::CallMemberOr {
            object: self.font,
            key,
            arguments: vec![text],
            result_needed: true,
            continuation: self,
        })
    }
    fn done(&self) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Int(self.mid as i64)))
    }
}
impl NativeContinuation for TextRange {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        let width = integer(cx, result)?;
        match self.phase {
            TextPhase::Full => {
                if width <= self.width {
                    return Ok(NativeStep::Return(Value::Void));
                }
                self.width = self.width.wrapping_sub(self.omit);
                let mid = self.mid;
                return self.measure(cx, 0, mid, TextPhase::Initial);
            }
            TextPhase::Initial => self.total = width,
            TextPhase::Add => self.total = self.total.wrapping_add(width),
            TextPhase::Subtract => self.total = self.total.wrapping_sub(width),
            TextPhase::Tail => {
                self.total = self.total.wrapping_add(width);
                if self.total > self.width {
                    return self.done();
                }
                self.mid += 1;
                self.index += 1;
                if self.index >= self.end {
                    return self.done();
                }
                let index = self.index;
                return self.measure(cx, index, index + 1, TextPhase::Tail);
            }
        }
        if self.begin >= self.end {
            return self.done();
        }
        if self.total <= self.width {
            if self.end - self.mid < 4 {
                // This deliberately starts at mid + 1, as in table.cpp. Do not
                // replace the source's measurements with a corrected prefix fit.
                self.index = self.mid + 1;
                if self.index >= self.end {
                    return self.done();
                }
                let index = self.index;
                return self.measure(cx, index, index + 1, TextPhase::Tail);
            }
            self.begin = self.mid;
            self.mid = self.begin + (self.end - self.begin) / 2;
            let (begin, mid) = (self.begin, self.mid);
            self.measure(cx, begin, mid, TextPhase::Add)
        } else {
            self.end = self.mid;
            self.mid = self.begin + (self.end - self.begin) / 2;
            let (mid, end) = (self.mid, self.end);
            self.measure(cx, mid, end, TextPhase::Subtract)
        }
    }
}
fn text_range(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arg(args, 3)?;
    let text = value::to_string_units(cx.heap(), args[1])?;
    let width = integer(cx, args[2])?;
    let omit = integer(cx, args[3])?;
    let font = receiver(args[0])?;
    if text.len() > TEXT_LIMIT {
        return Err(NativeError::Message("table text exceeds size limit"));
    }
    let end = text.len();
    Box::new(TextRange {
        font,
        text,
        width,
        omit,
        begin: 0,
        end,
        mid: end / 2,
        total: 0,
        index: 0,
        phase: TextPhase::Full,
    })
    .measure(cx, 0, end, TextPhase::Full)
}
