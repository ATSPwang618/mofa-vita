//! Single-threaded managed storage. IDs belong to their originating Heap and do
//! not keep entries alive. Collect only between execution slices/native calls.

mod arena;
mod arrays;
mod functions;
mod gc;
pub(crate) mod lifecycle;
mod members;
mod missing;
mod native;
mod strings;
mod symbols;
mod types;

use std::{cell::Cell, collections::HashMap, fmt::Write, rc::Rc};

use arena::Arena;
pub use gc::{CollectionPhase, CollectionStep};
use rustc_hash::FxHashMap;
use slotmap::{Key, SlotMap, new_key_type};

use crate::native::{
    NativeCallable, NativeClass, NativeError, NativeProperty, NativeState, NativeStorage, Trace,
};
use crate::{FunctionId, FunctionKind, Module, Value};

new_key_type! {
    struct ObjectKey;
    struct StringKey;
    struct StringBufferKey;
    struct OctetKey;
    struct SymbolKey;
    struct RootKey;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjId(ObjectKey);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StrId(StringKey);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OctetId(OctetKey);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SymbolId(SymbolKey);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RootId(RootKey);

/// Both the callable/receiver and its bound context are strong GC edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObjRef {
    pub object: Option<ObjId>,
    pub this: Option<ObjId>,
}

impl From<ObjId> for ObjRef {
    fn from(object: ObjId) -> Self {
        Self {
            object: Some(object),
            this: None,
        }
    }
}

impl ObjRef {
    pub fn bound(object: ObjId) -> Self {
        Self {
            object: Some(object),
            this: Some(object),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeakObjId(ObjId);

impl ObjId {
    pub fn downgrade(self) -> WeakObjId {
        WeakObjId(self)
    }
}

impl WeakObjId {
    pub fn upgrade(self, heap: &Heap) -> Option<ObjId> {
        heap.weak_object(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HeapError {
    #[error("managed object handle is no longer valid")]
    StaleObject,
    #[error("object has been invalidated")]
    InvalidObject,
    #[error("managed string handle is no longer valid")]
    StaleString,
    #[error("managed octet handle is no longer valid")]
    StaleOctet,
    #[error("managed symbol handle is no longer valid")]
    StaleSymbol,
    #[error("managed root handle is no longer valid")]
    StaleRoot,
    #[error("object is not an array")]
    NotArray,
    #[error("array index is out of bounds")]
    ArrayIndex,
    #[error("array allocation failed")]
    ArrayAllocation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Plain,
    Scope,
    Array,
    Dictionary,
    Function,
    Class,
    Property,
    /// VM-owned function constants; never exposed as a script value.
    FunctionPool,
    NativeClass,
    NativeFunction,
    NativeProperty,
}

enum ObjectData {
    Plain,
    Scope {
        this: ObjId,
        global: ObjId,
    },
    Array(Vec<Value>),
    Dictionary,
    Function(ScriptFunction),
    FunctionPool(Vec<Value>),
    NativeClass(&'static NativeClass),
    NativeFunction(NativeCallable),
    NativeProperty {
        descriptor: &'static NativeProperty,
        get: Option<ObjId>,
        set: Option<ObjId>,
    },
    NativeConstructor(ObjId),
}

pub(crate) struct ScriptFunction {
    pub module: Module,
    pub function: FunctionId,
    pub global: ObjId,
    pub pool: ObjId,
}

impl ScriptFunction {
    pub fn kind(&self) -> &FunctionKind {
        self.module.functions()[self.function.0 as usize].kind()
    }
}

pub struct ObjRecord {
    // Local overrides and deletions sit above a construction-time native snapshot.
    // These keys are heap-assigned numeric IDs, never user-controlled hashes.
    members: FxHashMap<SymbolId, Option<members::Member>>,
    shared_members: Option<Rc<members::Table>>,
    data: ObjectData,
    class_names: Vec<SymbolId>,
    native: Vec<Option<Box<dyn NativeState>>>,
    life: lifecycle::Life,
    finalizing: std::rc::Weak<()>,
    gc_finalized: bool,
    // Instances keep shared members, but only snapshot owners need cache invalidation.
    native_snapshot: bool,
    gray: Cell<bool>,
    missing: Option<std::rc::Weak<()>>,
}

impl ObjRecord {
    pub(crate) fn script_function(&self) -> Option<&ScriptFunction> {
        match &self.data {
            ObjectData::Function(function) => Some(function),
            _ => None,
        }
    }
    pub(crate) fn ensure_valid(&self) -> Result<(), HeapError> {
        if self.life == lifecycle::Life::Invalid {
            Err(HeapError::InvalidObject)
        } else {
            Ok(())
        }
    }
    pub fn kind(&self) -> ObjectKind {
        match self.data {
            ObjectData::Plain => ObjectKind::Plain,
            ObjectData::Scope { .. } => ObjectKind::Scope,
            ObjectData::Array(_) => ObjectKind::Array,
            ObjectData::Dictionary => ObjectKind::Dictionary,
            ObjectData::Function(ref function) => match function.kind() {
                FunctionKind::Function | FunctionKind::Internal | FunctionKind::SuperResolver => {
                    ObjectKind::Function
                }
                FunctionKind::Class { .. } => ObjectKind::Class,
                FunctionKind::Property { .. } => ObjectKind::Property,
            },
            ObjectData::FunctionPool(_) => ObjectKind::FunctionPool,
            ObjectData::NativeClass(_) => ObjectKind::NativeClass,
            ObjectData::NativeFunction(_) | ObjectData::NativeConstructor(_) => {
                ObjectKind::NativeFunction
            }
            ObjectData::NativeProperty { .. } => ObjectKind::NativeProperty,
        }
    }

    /// Storage iteration only; this is not TJS's observable enumeration contract.
    pub fn members(&self) -> impl Iterator<Item = (SymbolId, Value)> + '_ {
        self.entries().map(|(name, member)| (name, member.value))
    }
}

struct Entry<T> {
    // Interior mutability is confined to collector bookkeeping. It lets tracing
    // borrow object edges without allocating a temporary copy of every container.
    marked: Cell<bool>,
    data: T,
}

impl<T> Entry<T> {
    fn new(data: T, color: bool) -> Self {
        Self {
            marked: Cell::new(color),
            data,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapCounts {
    pub objects: usize,
    pub strings: usize,
    pub octets: usize,
    pub symbols: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct CollectionStats {
    pub before: HeapCounts,
    pub after: HeapCounts,
    pub traced_objects: usize,
    /// Estimated retained managed storage, including container capacities.
    /// Incremental cycles estimate each starting entry when it is swept; new
    /// allocations remain in allocation_debt rather than this estimate.
    /// Excludes module code, allocator overhead and external native resources.
    pub retained_bytes: usize,
}

/// Mutable Value containers stay private behind the collector write barrier. Collection is explicit; allocating cannot collect Rust temporaries.
#[derive(Default)]
pub struct Heap {
    objects: Arena<ObjectKey, Entry<ObjRecord>>,
    strings: Arena<StringKey, Entry<strings::Data>>,
    string_buffers: Arena<StringBufferKey, Entry<strings::Buffer>>,
    octets: Arena<OctetKey, Entry<Box<[u8]>>>,
    symbols: Arena<SymbolKey, Entry<Rc<[u16]>>>,
    symbol_index: HashMap<Rc<[u16]>, SymbolId>,
    string_symbols: symbols::Cache,
    roots: SlotMap<RootKey, Value>,
    gray: Vec<ObjId>,
    gc: gc::Collector,
    allocation_debt: usize,
    has_scopes: bool,
    native_classes: HashMap<&'static str, ObjId>,
    native_invalidators: HashMap<std::any::TypeId, crate::NativeCallable>,
    native_snapshots: HashMap<ObjId, Rc<members::Table>>,
    nested_classes: std::collections::HashSet<ObjId>,
    pub(crate) storage: Option<Box<dyn crate::storage::Storage>>,
    array_class: Option<ObjId>,
    dictionary_class: Option<ObjId>,
    pending_finalizers: std::collections::VecDeque<ObjId>,
}

impl Heap {
    pub fn new() -> Self {
        Self::default()
    }

    /// TJS exposes diagnostic object/context identities through string casts.
    /// Stable generational IDs replace C++ process pointers.
    pub fn object_text(&self, reference: ObjRef) -> String {
        let id = |object: Option<ObjId>| object.map_or(0, |id| id.0.data().as_ffi());
        format!(
            "(object 0x{:016x}:0x{:016x})",
            id(reference.object),
            id(reference.this)
        )
    }

    pub fn alloc_string(&mut self, units: impl Into<Box<[u16]>>) -> StrId {
        let units = units.into();
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(size_of_val(units.as_ref()) + size_of::<Entry<strings::Data>>());
        StrId(
            self.strings
                .insert(Entry::new(strings::Data::Owned(units), self.gc.color)),
        )
    }

    pub fn string(&self, id: StrId) -> Result<&[u16], HeapError> {
        self.strings
            .get(id.0)
            .map(|entry| match &entry.data {
                strings::Data::Owned(units) => units.as_ref(),
                strings::Data::Prefix { buffer, length } => {
                    &self.string_buffers[*buffer].data.units[..*length]
                }
            })
            .ok_or(HeapError::StaleString)
    }

    pub fn alloc_octet(&mut self, bytes: impl Into<Box<[u8]>>) -> OctetId {
        let bytes = bytes.into();
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(bytes.len() + size_of::<Entry<Box<[u8]>>>());
        OctetId(self.octets.insert(Entry::new(bytes, self.gc.color)))
    }

    pub fn octet(&self, id: OctetId) -> Result<&[u8], HeapError> {
        self.octets
            .get(id.0)
            .map(|entry| entry.data.as_ref())
            .ok_or(HeapError::StaleOctet)
    }

    pub fn intern(&mut self, name: &[u16]) -> SymbolId {
        if let Some(&id) = self.symbol_index.get(name) {
            return id;
        }
        let name: Rc<[u16]> = name.into();
        self.insert_symbol(name)
    }

    pub(crate) fn find_symbol(&self, name: &[u16]) -> Option<SymbolId> {
        self.symbol_index.get(name).copied()
    }

    pub fn intern_string(&mut self, id: StrId) -> Result<SymbolId, HeapError> {
        if let Some(symbol) = self.find_string_symbol(id)? {
            return Ok(symbol);
        }
        let units = Rc::from(crate::string::c_string(self.string(id)?));
        let symbol = self.insert_symbol(units);
        self.string_symbols.remember(id, symbol);
        Ok(symbol)
    }

    fn insert_symbol(&mut self, name: Rc<[u16]>) -> SymbolId {
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(size_of_val(name.as_ref()) + size_of::<Entry<Rc<[u16]>>>());
        let id = SymbolId(
            self.symbols
                .insert(Entry::new(Rc::clone(&name), self.gc.color)),
        );
        self.symbol_index.insert(name, id);
        id
    }

    pub fn symbol(&self, id: SymbolId) -> Result<&[u16], HeapError> {
        self.symbols
            .get(id.0)
            .map(|entry| entry.data.as_ref())
            .ok_or(HeapError::StaleSymbol)
    }

    pub fn alloc_object(&mut self) -> ObjId {
        self.alloc_record(ObjectData::Plain)
    }
    pub fn alloc_array(&mut self) -> ObjId {
        if let Some(class) = self.array_class {
            let literal = self
                .native_class(class)
                .expect("registered class")
                .literal
                .expect("intrinsic class has a literal factory");
            return literal(self, class);
        }
        self.alloc_record(ObjectData::Array(Vec::new()))
    }

    pub fn alloc_array_from(&mut self, values: &[Value]) -> Result<ObjId, HeapError> {
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(values.len())
            .map_err(|_| HeapError::ArrayAllocation)?;
        owned.extend_from_slice(values);
        self.allocation_debt = self.allocation_debt.saturating_add(size_of_val(values));
        let id = self.alloc_array();
        self.object_mut(id)?.data = ObjectData::Array(owned);
        Ok(id)
    }
    pub fn alloc_dictionary(&mut self) -> ObjId {
        if let Some(class) = self.dictionary_class {
            let literal = self
                .native_class(class)
                .expect("registered class")
                .literal
                .expect("intrinsic class has a literal factory");
            return literal(self, class);
        }
        self.alloc_record(ObjectData::Dictionary)
    }

    pub(crate) fn alloc_function(&mut self, function: ScriptFunction) -> ObjId {
        self.alloc_record(ObjectData::Function(function))
    }

    pub(crate) fn function(&self, id: ObjId) -> Result<Option<&ScriptFunction>, HeapError> {
        Ok(self.object(id)?.script_function())
    }

    fn alloc_record(&mut self, data: ObjectData) -> ObjId {
        self.allocation_debt = self.allocation_debt.saturating_add(size_of::<ObjRecord>());
        let id = ObjId(self.objects.insert(Entry::new(
            ObjRecord {
                members: FxHashMap::default(),
                data,
                shared_members: None,
                class_names: Vec::new(),
                native: Vec::new(),
                life: lifecycle::Life::Live,
                finalizing: std::rc::Weak::new(),
                gc_finalized: false,
                native_snapshot: false,
                gray: Cell::new(false),
                missing: None,
            },
            self.gc.color,
        )));
        self.write_barrier(id);
        id
    }

    pub fn object(&self, id: ObjId) -> Result<&ObjRecord, HeapError> {
        self.objects
            .get(id.0)
            .map(|entry| &entry.data)
            .ok_or(HeapError::StaleObject)
    }

    fn object_mut(&mut self, id: ObjId) -> Result<&mut ObjRecord, HeapError> {
        self.write_barrier(id);
        self.object_mut_unbarriered(id)
    }

    // Callers must shade newly stored edges before using this mutation path.
    fn object_mut_unbarriered(&mut self, id: ObjId) -> Result<&mut ObjRecord, HeapError> {
        self.objects
            .get_mut(id.0)
            .map(|entry| &mut entry.data)
            .ok_or(HeapError::StaleObject)
    }

    fn valid_object_mut(&mut self, id: ObjId) -> Result<&mut ObjRecord, HeapError> {
        let object = self.object_mut(id)?;
        object.ensure_valid()?;
        Ok(object)
    }

    pub fn array_push(&mut self, id: ObjId, value: Value) -> Result<(), HeapError> {
        self.edge_barrier(id, [value]);
        let array = self.array_mut(id)?;
        array
            .try_reserve(1)
            .map_err(|_| HeapError::ArrayAllocation)?;
        array.push(value);
        self.allocation_debt = self.allocation_debt.saturating_add(size_of::<Value>());
        Ok(())
    }

    pub fn array_set(&mut self, id: ObjId, index: usize, value: Value) -> Result<(), HeapError> {
        self.edge_barrier(id, [value]);
        *self
            .array_mut(id)?
            .get_mut(index)
            .ok_or(HeapError::ArrayIndex)? = value;
        Ok(())
    }

    pub fn array_resize(&mut self, id: ObjId, length: usize) -> Result<(), HeapError> {
        let array = self.array_mut(id)?;
        let added = length.saturating_sub(array.len());
        array
            .try_reserve(added)
            .map_err(|_| HeapError::ArrayAllocation)?;
        array.resize(length, Value::Void);
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(added.saturating_mul(size_of::<Value>()));
        Ok(())
    }

    pub fn array_remove(&mut self, id: ObjId, index: usize) -> Result<Value, HeapError> {
        let array = self.array_mut(id)?;
        if index >= array.len() {
            return Err(HeapError::ArrayIndex);
        }
        Ok(array.remove(index))
    }

    /// Remove ascending unique indices in one stable compaction, retaining the
    /// existing allocation. Validate before mutating so errors are atomic.
    pub fn array_remove_indices(&mut self, id: ObjId, indices: &[usize]) -> Result<(), HeapError> {
        let array = self.array_mut(id)?;
        if indices.last().is_some_and(|&i| i >= array.len())
            || indices.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(HeapError::ArrayIndex);
        }
        if indices.is_empty() {
            return Ok(());
        }
        let mut write = indices[0];
        let mut read = write + 1;
        for &removed in &indices[1..] {
            array.copy_within(read..removed, write);
            write += removed - read;
            read = removed + 1;
        }
        let tail = array.len() - read;
        array.copy_within(read.., write);
        array.truncate(write + tail);
        Ok(())
    }

    pub fn array_insert(
        &mut self,
        id: ObjId,
        index: usize,
        values: &[Value],
    ) -> Result<(), HeapError> {
        self.edge_barrier(id, values.iter().copied());
        let array = self.array_mut(id)?;
        if index > array.len() {
            return Err(HeapError::ArrayIndex);
        }
        array
            .try_reserve(values.len())
            .map_err(|_| HeapError::ArrayAllocation)?;
        array.splice(index..index, values.iter().copied());
        self.allocation_debt = self.allocation_debt.saturating_add(size_of_val(values));
        Ok(())
    }

    /// Replace the element buffer at one heap mutation boundary.
    pub fn array_replace(&mut self, id: ObjId, values: Vec<Value>) -> Result<(), HeapError> {
        self.edge_barrier(id, values.iter().copied());
        let array = self.array_mut(id)?;
        let added = values.len().saturating_sub(array.len());
        *array = values;
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(added * size_of::<Value>());
        Ok(())
    }

    pub fn array_reverse(&mut self, id: ObjId) -> Result<(), HeapError> {
        let array = self.array_mut(id)?;
        array.reverse();
        Ok(())
    }

    /// Explicit host roots remain alive until release_root, even if the RootId is
    /// dropped. A copied Value/ObjId alone is never a root.
    pub fn root(&mut self, value: Value) -> RootId {
        RootId(self.roots.insert(value))
    }
    pub fn rooted(&self, root: RootId) -> Option<Value> {
        self.roots.get(root.0).copied()
    }
    pub fn set_root(&mut self, root: RootId, value: Value) -> Result<(), HeapError> {
        *self.roots.get_mut(root.0).ok_or(HeapError::StaleRoot)? = value;
        Ok(())
    }
    pub fn release_root(&mut self, root: RootId) -> Option<Value> {
        self.roots.remove(root.0)
    }

    pub fn counts(&self) -> HeapCounts {
        HeapCounts {
            objects: self.objects.len(),
            strings: self.strings.len(),
            octets: self.octets.len(),
            symbols: self.symbols.len(),
        }
    }

    /// Payload/record allocation debt is a collection trigger, not a measurement
    /// of allocator or container overhead and not an OOM budget.
    pub fn allocation_debt(&self) -> usize {
        self.allocation_debt
    }

    /// SlotMap keeps backing capacity after sweep; expose it for memory studies.
    pub fn capacities(&self) -> HeapCounts {
        HeapCounts {
            objects: self.objects.capacity(),
            strings: self.strings.capacity(),
            octets: self.octets.capacity(),
            symbols: self.symbols.capacity(),
        }
    }

    /// Host display, not TJS's implicit string conversion. Lone surrogates are
    /// escaped for a UTF-8 terminal without modifying the stored code units.
    pub fn display(&self, value: Value) -> Result<String, HeapError> {
        if let Value::Str(id) = value {
            let mut text = String::new();
            for character in char::decode_utf16(self.string(id)?.iter().copied()) {
                match character {
                    Ok(c) => text.push(c),
                    Err(e) => write!(&mut text, "\\u{{{:04X}}}", e.unpaired_surrogate())
                        .expect("String formatting"),
                }
            }
            Ok(text)
        } else if let Value::Octet(id) = value {
            let bytes = self.octet(id)?;
            let mut text = String::with_capacity(5 + bytes.len() * 3);
            text.push_str("<%");
            for byte in bytes {
                write!(&mut text, " {byte:02x}").expect("String formatting");
            }
            text.push_str(" %>");
            Ok(text)
        } else {
            Ok(value.to_string())
        }
    }
}
