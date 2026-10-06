//! TJS-facing KAGParser. The independent parser owns syntax/control state;
//! this adapter owns managed dictionaries, callbacks and resumable resource IO.
mod advanced;
mod bindings;
mod extended;
mod save;
mod task;
use bindings::implementation::State;
pub use krkr_kag as parser;
use krkr_kag::{Parser, Scenario, Text, units};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};
use tjs_core::{
    Heap, NativeCx, NativeError, NativeResult, ObjId, ObjRef, SymbolId, Trace, Value, value,
};

#[derive(Default)]
struct Cache {
    entries: VecDeque<(Text, Arc<Scenario>)>,
    bytes: usize,
}
impl Cache {
    fn get(&mut self, name: &[u16]) -> Option<Arc<Scenario>> {
        let at = self.entries.iter().position(|(key, _)| key == name)?;
        let entry = self.entries.remove(at).unwrap();
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }
    fn insert(&mut self, name: Text, scenario: Arc<Scenario>) {
        let bytes = scenario.source_bytes();
        if bytes > 32 * 1024 * 1024 {
            return;
        }
        while self.entries.len() >= 8 || self.bytes + bytes > 32 * 1024 * 1024 {
            let (_, old) = self.entries.pop_front().unwrap();
            self.bytes -= old.source_bytes();
        }
        self.bytes += bytes;
        self.entries.push_back((name, scenario));
    }
}
struct Keys {
    // Symbols are weak handles. Keep their names in a traced member table.
    names: ObjId,
    tagname: SymbolId,
    text: SymbolId,
    eol: SymbolId,
    ch: Value,
    r: Value,
    interrupt: Value,
    yes: Value,
}
impl Keys {
    fn new(heap: &mut Heap) -> NativeResult<Self> {
        let tagname = heap.intern(&units("tagname"));
        let text = heap.intern(&units("text"));
        let eol = heap.intern(&units("eol"));
        let names = heap.alloc_object();
        for key in [tagname, text, eol] {
            heap.set_member(names, key, Value::Void)?;
        }
        Ok(Self {
            names,
            tagname,
            text,
            eol,
            ch: string(heap, units("ch")),
            r: string(heap, units("r")),
            interrupt: string(heap, units("interrupt")),
            yes: string(heap, units("true")),
        })
    }
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.names.into()));
        for v in [self.ch, self.r, self.interrupt, self.yes] {
            visit(v);
        }
    }
}
fn error(error: krkr_kag::Error) -> NativeError {
    NativeError::Detail(error.to_string())
}
fn text(cx: &mut NativeCx<'_>, val: Value) -> NativeResult<Text> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), val)? else {
        unreachable!()
    };
    Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
}
fn string(heap: &mut Heap, text: Text) -> Value {
    Value::Str(heap.alloc_string(text))
}
fn object(value: Value) -> NativeResult<ObjId> {
    match value {
        Value::Obj(ObjRef {
            object: Some(id), ..
        }) => Ok(id),
        _ => Err(NativeError::Type("an object")),
    }
}
fn put(heap: &mut Heap, id: ObjId, key: &[u16], value: Value) -> NativeResult<()> {
    let key = heap.intern(key);
    heap.set_member(id, key, value)?;
    Ok(())
}
fn get(heap: &mut Heap, id: ObjId, key: &[u16]) -> NativeResult<Value> {
    let key = heap.intern(key);
    Ok(heap.member(id, key)?.unwrap_or(Value::Void))
}
fn copy(heap: &mut Heap, source: ObjId, target: ObjId, clear: bool) -> NativeResult<()> {
    let fields = heap.members(source)?.collect::<Vec<_>>();
    if clear {
        heap.clear_members(target)?;
    }
    for (name, value) in fields {
        heap.set_member(target, name, value)?;
    }
    Ok(())
}
fn clone_dictionary(heap: &mut Heap, source: ObjId) -> NativeResult<ObjId> {
    let target = heap.alloc_dictionary();
    copy(heap, source, target, true)?;
    Ok(target)
}

impl State {
    fn tag(&self) -> NativeResult<ObjId> {
        self.tag.ok_or(NativeError::This)
    }
    fn macros_id(&self) -> NativeResult<ObjId> {
        self.macros.ok_or(NativeError::This)
    }
    fn set_tag(&mut self, cx: &mut NativeCx<'_>, name: Text) -> NativeResult<()> {
        let id = self.tag()?;
        cx.heap_mut().clear_members(id)?;
        if let Some(order) = self.tag_order {
            cx.heap_mut().array_resize(order, 0)?;
        }
        let value = string(cx.heap_mut(), name);
        cx.heap_mut()
            .set_member(id, self.keys.as_ref().unwrap().tagname, value)?;
        self.record_key(cx.heap_mut(), &units("tagname"))?;
        Ok(())
    }
    fn simple(&mut self, cx: &mut NativeCx<'_>, token: krkr_kag::Token) -> NativeResult<Value> {
        let id = self.tag()?;
        let keys = self.keys.as_ref().unwrap();
        cx.heap_mut().clear_members(id)?;
        if let Some(order) = self.tag_order {
            cx.heap_mut().array_resize(order, 0)?;
        }
        let name = match token {
            krkr_kag::Token::Character(ch) => {
                let low = self.parser.line().get(self.parser.position.pos).copied();
                let value = if (self.extended || self.advanced.is_some())
                    && (0xd800..=0xdbff).contains(&ch)
                    && low.is_some_and(|u| (0xdc00..=0xdfff).contains(&u))
                {
                    self.parser.position.pos += 1;
                    string(cx.heap_mut(), vec![ch, low.unwrap()])
                } else if let Some(&value) = self.characters.get(&ch) {
                    value
                } else {
                    if self.characters.len() >= 1024 {
                        self.characters.clear();
                    }
                    let value = string(cx.heap_mut(), vec![ch]);
                    self.characters.insert(ch, value);
                    value
                };
                cx.heap_mut().set_member(id, keys.text, value)?;
                keys.ch
            }
            krkr_kag::Token::Newline(eol) => {
                if eol {
                    cx.heap_mut().set_member(id, keys.eol, keys.yes)?;
                }
                keys.r
            }
            krkr_kag::Token::Interrupt => keys.interrupt,
            _ => unreachable!(),
        };
        cx.heap_mut().set_member(id, keys.tagname, name)?;
        let extra = if cx.heap().member(id, keys.text)?.is_some() {
            Some("text")
        } else if cx.heap().member(id, keys.eol)?.is_some() {
            Some("eol")
        } else {
            None
        };
        self.record_key(cx.heap_mut(), &units("tagname"))?;
        if let Some(extra) = extra {
            self.record_key(cx.heap_mut(), &units(extra))?;
        }
        self.return_tag(cx.heap_mut())
    }
    fn push_args(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
        let depth = self.parser.macro_depth;
        if depth >= self.parser.max_depth {
            return Err(NativeError::Message("macro argument depth limit"));
        }
        if self.args.len() == depth {
            let id = cx.heap_mut().alloc_dictionary();
            self.args.push(id);
        }
        copy(cx.heap_mut(), self.tag()?, self.args[depth], true)?;
        if let Some(order) = self.tag_order {
            let values = cx.heap().array(order)?.to_vec();
            if self.arg_order.len() == depth {
                self.arg_order.push(cx.heap_mut().alloc_array());
            }
            cx.heap_mut().array_replace(self.arg_order[depth], values)?;
        }
        self.parser.macro_depth += 1;
        Ok(())
    }
    fn params_id(&self) -> Option<ObjId> {
        self.parser
            .macro_depth
            .checked_sub(1)
            .and_then(|at| self.args.get(at))
            .copied()
    }
    fn record_key(&self, heap: &mut Heap, name: &[u16]) -> NativeResult<()> {
        if let Some(order) = self.tag_order {
            let name = string(heap, name.to_vec());
            heap.array_push(order, name)?;
        }
        Ok(())
    }
    fn put_tag(&self, heap: &mut Heap, name: &[u16], value: Value) -> NativeResult<()> {
        put(heap, self.tag()?, name, value)?;
        self.record_key(heap, name)
    }
    fn return_tag(&self, heap: &mut Heap) -> NativeResult<Value> {
        if let Some(order) = self.tag_order {
            put(
                heap,
                self.tag()?,
                &units("taglist"),
                Value::Obj(ObjRef::bound(order)),
            )?;
        }
        Ok(Value::Obj(ObjRef::bound(self.tag()?)))
    }
}

/// Install the plugin variant without replacing the engine registry's base class.
pub fn install_extended(heap: &mut Heap) -> NativeResult<ObjId> {
    extended::install(heap)
}
pub fn install_advanced(heap: &mut Heap) -> NativeResult<ObjId> {
    advanced::install(heap)
}

pub fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    if heap.registered_class("Scripts").is_none() {
        crate::scripts::install(heap)?;
    }
    let class = bindings::implementation::install(heap)?;
    heap.initialize_class_state::<State>(class)?;
    Ok(class)
}
pub(crate) fn attach(heap: &mut Heap, operations: crate::operations::Shared) -> NativeResult<()> {
    if let Some(class) = heap.registered_class("KAGParser") {
        heap.with_native_state::<State, _>(class, |state| state.operations = Some(operations))?;
    }
    Ok(())
}
