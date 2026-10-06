//! Deprecated MenuItem script compatibility. There is deliberately no native UI.
use std::rc::{Rc, Weak};
use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef,
    RestArgs, Trace, Value, value,
};

// These bound synchronous tree walks and snapshot allocation, not game duration.
const MAX_CHILDREN: usize = 1024;
const MAX_DEPTH: usize = 128;

struct Item {
    owner: Value,
    click: Value,
    action: Value,
    window: Option<ObjId>,
    parent: Option<ObjId>,
    children: Vec<ObjId>,
    array: Option<ObjId>,
    dirty: bool,
    caption: Vec<u16>,
    shortcut: Vec<u16>,
    checked: bool,
    enabled: bool,
    visible: bool,
    radio: bool,
    group: i32,
    disposing: Weak<()>,
}
impl Item {
    fn new(heap: &mut Heap, owner: Value) -> NativeResult<Self> {
        if !matches!(owner, Value::Obj(_)) {
            return Err(NativeError::Type("an action owner object"));
        }
        Ok(Self {
            owner,
            click: Value::Str(heap.alloc_string("onClick".encode_utf16().collect::<Vec<_>>())),
            action: Value::Str(heap.alloc_string("action".encode_utf16().collect::<Vec<_>>())),
            window: None,
            parent: None,
            children: Vec::new(),
            array: None,
            dirty: true,
            caption: Vec::new(),
            shortcut: Vec::new(),
            checked: false,
            enabled: true,
            visible: true,
            radio: false,
            group: 0,
            disposing: Weak::new(),
        })
    }
    fn mutable(&self) -> NativeResult<()> {
        if self.disposing.strong_count() != 0 {
            return Err(NativeError::Message("menu is being invalidated"));
        }
        Ok(())
    }
}
impl Trace for Item {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.click.trace(visit);
        self.action.trace(visit);
        self.window.trace(visit);
        self.parent.trace(visit);
        self.children.trace(visit);
        self.array.trace(visit);
    }
}
fn object(id: Option<ObjId>) -> Value {
    Value::Obj(id.map(ObjRef::bound).unwrap_or_default())
}
fn with<R>(heap: &mut Heap, id: ObjId, f: impl FnOnce(&mut Item) -> R) -> NativeResult<R> {
    heap.with_native_state::<implementation::State, _>(id, |s| {
        s.item.as_mut().map(f).ok_or(NativeError::This)
    })?
}
fn menu_id(heap: &mut Heap, v: Value) -> NativeResult<ObjId> {
    let Value::Obj(ObjRef {
        object: Some(id), ..
    }) = v
    else {
        return Err(NativeError::Type("a MenuItem object"));
    };
    with(heap, id, |_| ())?;
    Ok(id)
}
fn text(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), v)? else {
        unreachable!()
    };
    Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
}

/// Removes only the relationship; invalidation and moving are separate operations.
fn detach(heap: &mut Heap, child: ObjId) -> NativeResult<()> {
    if let Some(parent) = with(heap, child, |s| s.parent)? {
        with(heap, parent, |s| {
            s.children.retain(|&id| id != child);
            s.dirty = true;
        })?;
        with(heap, child, |s| s.parent = None)?;
    }
    Ok(())
}
fn insert(heap: &mut Heap, parent: ObjId, child: ObjId, index: Option<i32>) -> NativeResult<()> {
    let (old_parent, window) = with(heap, child, |s| -> NativeResult<_> {
        s.mutable()?;
        Ok((s.parent, s.window))
    })??;
    with(heap, parent, |s| s.mutable())??;
    if window.is_some() {
        return Err(NativeError::Message("a window menu cannot be a child"));
    }
    // Reject multi-parent ownership instead of copying the reference's dangling pointers.
    if old_parent.is_some_and(|p| p != parent) {
        return Err(NativeError::Message(
            "remove the menu from its parent before inserting it",
        ));
    }
    let mut ancestor = Some(parent);
    let mut depth = 0;
    while let Some(id) = ancestor {
        if id == child {
            return Err(NativeError::Message("menu tree cannot contain a cycle"));
        }
        depth += 1;
        if depth >= MAX_DEPTH {
            return Err(NativeError::Message("menu tree depth limit reached"));
        }
        ancestor = with(heap, id, |s| s.parent)?;
    }
    // A detached subtree can have depth of its own. The bounded DFS retains one
    // cursor per level, rather than cloning every descendant's children.
    let mut stack = vec![(child, 0usize)];
    let mut visited = 1usize;
    while let Some(&(id, next)) = stack.last() {
        if depth + stack.len() > MAX_DEPTH {
            return Err(NativeError::Message("menu tree depth limit reached"));
        }
        if let Some(node) = with(heap, id, |s| s.children.get(next).copied())? {
            visited += 1;
            if visited > MAX_CHILDREN {
                return Err(NativeError::Message("menu subtree capacity reached"));
            }
            stack.last_mut().expect("one cursor").1 += 1;
            stack.push((node, 0));
        } else {
            stack.pop();
        }
    }
    with(heap, parent, |s| {
        let count = s.children.len();
        let index = index.map(|i| i as usize);
        if index.is_some_and(|i| i > count) {
            return Err(NativeError::Message("menu index is out of range"));
        }
        if old_parent == Some(parent) {
            return Ok(()); // Adding an existing child does not duplicate or move it.
        }
        if count >= MAX_CHILDREN {
            return Err(NativeError::Message("menu child capacity reached"));
        }
        s.children.insert(index.unwrap_or(count), child);
        s.dirty = true;
        Ok(())
    })??;
    with(heap, child, |s| s.parent = Some(parent))
}

#[tjs_bind::class(name = "MenuItem")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) item: Option<Item>,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.item.trace(visit);
        }
    }
    impl State {
        fn item(&self) -> NativeResult<&Item> {
            self.item.as_ref().ok_or(NativeError::This)
        }
        fn item_mut(&mut self) -> NativeResult<&mut Item> {
            let item = self.item.as_mut().ok_or(NativeError::This)?;
            item.mutable()?;
            Ok(item)
        }
        #[tjs::constructor]
        fn create(cx: &mut NativeCx<'_>, owner: Value, args: RestArgs<'_>) -> NativeResult<Self> {
            let this = cx.this();
            if cx
                .heap_mut()
                .with_native_state::<State, _>(this, |s| s.item.is_some())?
            {
                return Err(NativeError::Message("MenuItem is already constructed"));
            }
            let mut item = Item::new(cx.heap_mut(), owner)?;
            if let Some(&second) = args.first() {
                if let Value::Obj(o) = second {
                    let window = o.object.ok_or(NativeError::Type("a Window object"))?;
                    crate::window::bindings::attach_menu(cx.heap_mut(), window, this)?;
                    item.window = Some(window);
                } else {
                    item.caption = text(cx, second)?;
                }
            }
            Ok(Self { item: Some(item) })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate(resumable = true)]
        fn invalidate(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let id = cx.this();
            if !cx
                .heap_mut()
                .with_native_state::<State, _>(id, |s| s.item.is_some())?
            {
                return Ok(NativeStep::Return(Value::Void));
            }
            let lock = Rc::new(());
            detach(cx.heap_mut(), id)?;
            with(cx.heap_mut(), id, |s| s.disposing = Rc::downgrade(&lock))?;
            Box::new(Invalidating {
                id,
                pending: None,
                _lock: lock,
            })
            .next(cx)
        }
        #[tjs::method]
        fn add(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let this = cx.this();
            let child = menu_id(cx.heap_mut(), value)?;
            super::insert(cx.heap_mut(), this, child, None)
        }
        #[tjs::method]
        fn insert(cx: &mut NativeCx<'_>, value: Value, index: Value) -> NativeResult<()> {
            let this = cx.this();
            let child = menu_id(cx.heap_mut(), value)?;
            let index = value::to_integer(cx.heap(), index)? as i32;
            super::insert(cx.heap_mut(), this, child, Some(index))
        }
        #[tjs::method]
        fn remove(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let this = cx.this();
            with(cx.heap_mut(), this, |s| s.mutable())??;
            let child = menu_id(cx.heap_mut(), value)?;
            if with(cx.heap_mut(), child, |s| s.parent)? == Some(this) {
                detach(cx.heap_mut(), child)?;
            }
            Ok(())
        }
        #[tjs::getter]
        fn parent(&self) -> NativeResult<Value> {
            Ok(object(self.item()?.parent))
        }
        #[tjs::getter]
        fn window(&self) -> NativeResult<Value> {
            Ok(object(self.item()?.window))
        }
        #[tjs::getter]
        fn root(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let mut id = cx.this();
            while let Some(parent) = with(cx.heap_mut(), id, |s| s.parent)? {
                id = parent;
            }
            Ok(object(Some(id)))
        }
        #[tjs::getter]
        fn children(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let s = self.item.as_mut().ok_or(NativeError::This)?;
            let array = *s.array.get_or_insert_with(|| cx.heap_mut().alloc_array());
            if s.dirty {
                cx.heap_mut().array_replace(
                    array,
                    s.children.iter().map(|&id| object(Some(id))).collect(),
                )?;
                s.dirty = false;
            }
            Ok(object(Some(array)))
        }
        #[tjs::getter]
        fn index(&self, cx: &mut NativeCx<'_>) -> NativeResult<i64> {
            let id = cx.this();
            match self.item()?.parent {
                Some(parent) => with(cx.heap_mut(), parent, |s| {
                    s.children.iter().position(|&c| c == id).unwrap_or(0) as i64
                }),
                None => Ok(0),
            }
        }
        #[tjs::setter(name = "index")]
        fn set_index(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let s = self.item()?;
            s.mutable()?;
            let index = value::to_integer(cx.heap(), value)? as i32 as usize;
            let Some(parent) = s.parent else {
                return Ok(());
            };
            let id = cx.this();
            with(cx.heap_mut(), parent, |p| {
                p.mutable()?;
                if index >= p.children.len() {
                    return Err(NativeError::Message("menu index is out of range"));
                }
                let old = p
                    .children
                    .iter()
                    .position(|&c| c == id)
                    .ok_or(NativeError::This)?;
                p.children.remove(old);
                p.children.insert(index, id);
                p.dirty = true;
                Ok(())
            })?
        }
        #[tjs::getter]
        fn caption(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(Value::Str(
                cx.heap_mut().alloc_string(self.item()?.caption.clone()),
            ))
        }
        #[tjs::setter(name = "caption")]
        fn set_caption(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.item_mut()?.caption = text(cx, value)?;
            Ok(())
        }
        #[tjs::getter]
        fn shortcut(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            Ok(Value::Str(
                cx.heap_mut().alloc_string(self.item()?.shortcut.clone()),
            ))
        }
        #[tjs::setter(name = "shortcut")]
        fn set_shortcut(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.item_mut()?.shortcut = text(cx, value)?;
            Ok(())
        }
        #[tjs::getter]
        fn checked(&self) -> NativeResult<bool> {
            Ok(self.item()?.checked)
        }
        #[tjs::setter(name = "checked")]
        fn set_checked(&mut self, cx: &mut NativeCx<'_>, value: bool) -> NativeResult<()> {
            let this = cx.this();
            let s = self.item_mut()?;
            if value
                && s.radio
                && let Some(parent) = s.parent
            {
                let children = with(cx.heap_mut(), parent, |p| p.children.clone())?;
                for child in children.into_iter().filter(|&id| id != this) {
                    with(cx.heap_mut(), child, |other| {
                        if other.radio && other.group == s.group {
                            other.checked = false;
                        }
                    })?;
                }
            }
            s.checked = value;
            Ok(())
        }
        #[tjs::getter]
        fn enabled(&self) -> NativeResult<bool> {
            Ok(self.item()?.enabled)
        }
        #[tjs::setter(name = "enabled")]
        fn set_enabled(&mut self, v: bool) -> NativeResult<()> {
            self.item_mut()?.enabled = v;
            Ok(())
        }
        #[tjs::getter]
        fn visible(&self) -> NativeResult<bool> {
            Ok(self.item()?.visible)
        }
        #[tjs::setter(name = "visible")]
        fn set_visible(&mut self, v: bool) -> NativeResult<()> {
            self.item_mut()?.visible = v;
            Ok(())
        }
        #[tjs::getter]
        fn radio(&self) -> NativeResult<bool> {
            Ok(self.item()?.radio)
        }
        #[tjs::setter(name = "radio")]
        fn set_radio(&mut self, v: bool) -> NativeResult<()> {
            self.item_mut()?.radio = v;
            Ok(())
        }
        #[tjs::getter]
        fn group(&self) -> NativeResult<i64> {
            Ok(self.item()?.group.into())
        }
        #[tjs::setter(name = "group")]
        fn set_group(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            self.item_mut()?.group = value::to_integer(cx.heap(), v)? as i32;
            Ok(())
        }
        #[tjs::getter(name = "HMENU")]
        fn handle(&self) -> NativeResult<i64> {
            self.item()?;
            Ok(0)
        }
        #[tjs::method]
        fn popup(
            &self,
            cx: &mut NativeCx<'_>,
            flags: Value,
            x: Value,
            y: Value,
        ) -> NativeResult<i64> {
            self.item()?;
            for v in [flags, x, y] {
                value::to_integer(cx.heap(), v)?;
            }
            Ok(0)
        }
        #[tjs::method(name = "onClick", resumable = true)]
        fn on_click(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let s = self.item()?;
            if matches!(s.owner, Value::Obj(ObjRef { object: None, .. })) {
                return Ok(NativeStep::Return(Value::Void));
            }
            let fields = [
                cx.heap_mut().intern(&b"type".map(u16::from)),
                cx.heap_mut().intern(&b"target".map(u16::from)),
            ];
            crate::window::callbacks::action_for(cx, s.owner, s.click, s.action, &fields, &[], &[])
        }
        #[tjs::method(name = "fireClick", resumable = true)]
        fn fire_click(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let this = cx.this();
            let click = with(cx.heap_mut(), this, |s| s.click)?;
            let mut next = Some(this);
            let mut window = None;
            while let Some(id) = next {
                let (enabled, parent, attached, disposing) = with(cx.heap_mut(), id, |s| {
                    (
                        s.enabled,
                        s.parent,
                        s.window,
                        s.disposing.strong_count() != 0,
                    )
                })?;
                if !enabled || disposing {
                    return Ok(NativeStep::Return(Value::Void));
                }
                window = window.or(attached);
                next = parent;
            }
            if let Some(window) = window
                && crate::window::bindings::menu_events_allowed(cx.heap_mut(), window)?
            {
                return Ok(NativeStep::CallMember {
                    object: object(Some(this)),
                    key: click,
                    arguments: Vec::new(),
                    continuation: Box::new(Returned),
                });
            }
            Ok(NativeStep::Return(Value::Void))
        }
    }
}

struct Returned;
impl Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Void))
    }
}
struct Invalidating {
    id: ObjId,
    pending: Option<ObjId>,
    _lock: Rc<()>,
}
impl Trace for Invalidating {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.id.trace(visit);
        self.pending.trace(visit);
    }
}
impl Invalidating {
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if let Some(child) = with(cx.heap_mut(), self.id, |s| s.children.first().copied())? {
            self.pending = Some(child);
            Ok(NativeStep::Invalidate {
                object: object(Some(child)),
                continuation: self,
            })
        } else {
            cx.heap_mut()
                .with_native_state::<implementation::State, _>(self.id, |s| s.item = None)?;
            Ok(NativeStep::Return(Value::Void))
        }
    }
}
impl NativeContinuation for Invalidating {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        // A child finalizer may invalidate its own parent. In that case the VM
        // rejects reentrant child invalidation; detach it and let its active
        // finalizer finish, instead of repeatedly requesting the same child.
        if let Some(child) = self.pending {
            with(cx.heap_mut(), self.id, |s| {
                s.children.retain(|&id| id != child);
                s.dirty = true;
            })?;
            if cx.heap().is_valid(child)? {
                with(cx.heap_mut(), child, |s| {
                    if s.parent == Some(self.id) {
                        s.parent = None;
                    }
                })?;
            }
        }
        self.next(cx)
    }
}
pub(crate) fn root(heap: &mut Heap, window: ObjId) -> NativeResult<ObjId> {
    let class = heap
        .registered_class("MenuItem")
        .expect("installed MenuItem");
    let mut item = Item::new(heap, object(Some(window)))?;
    item.window = Some(window);
    heap.alloc_native(class, implementation::State { item: Some(item) })
}
pub(crate) fn install(heap: &mut Heap) -> NativeResult<()> {
    implementation::install(heap)?;
    Ok(())
}
