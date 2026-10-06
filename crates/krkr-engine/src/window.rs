//! Logical windows and script ownership. Native windows live exclusively in the host.
pub(crate) mod bindings;
pub(crate) mod callbacks;
pub(crate) mod clipboard;
pub(crate) mod icons;
mod lifetime;
mod modal;
mod presentation;
pub(crate) mod screen;
mod viewport;
use presentation::{Hints, Pending};
use std::time::Duration;
pub(crate) mod desktop;
pub(crate) mod draw_device;
mod extended;
pub(crate) mod extension;
pub(crate) mod input;
mod tasks;
use crate::{
    events::{self, Kind, SourceId},
    operations,
};
use krkr_protocol::window::{Client, Command, Geometry, Input, WindowId};
use slotmap::{SecondaryMap, SlotMap};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tjs_core::{Heap, NativeError, NativeResult, ObjId, Trace, Value};

pub(crate) type Shared = Rc<RefCell<Windows>>;
pub(crate) type Delivery = Rc<RefCell<Option<krkr_protocol::window::Response>>>;
pub(crate) struct Windows {
    pub system: std::rc::Weak<RefCell<crate::system::System>>,
    pub layers: std::rc::Weak<RefCell<crate::layer::Layers>>,
    pub host: Option<Client>,
    pub operations: operations::Shared,
    events: events::Shared,
    clock: Rc<dyn tjs_runtime::clock::Clock>,
    records: SlotMap<WindowId, Record>,
    sources: SecondaryMap<SourceId, WindowId>,
    main: Option<WindowId>,
    modals: Vec<modal::Entry>,
    names: Vec<Value>,
    pub(crate) fields: [tjs_core::SymbolId; 15],
    key_root: ObjId,
    post_keys: [Value; 2],
}
struct Record {
    registered: bool,
    owner: ObjId,
    source: SourceId,
    alive: Arc<AtomicBool>,
    geometry: Geometry,
    snapshot: Option<krkr_protocol::window::Snapshot>,
    icon: Option<Arc<krkr_protocol::window::IconImage>>,
    disable_resize: bool,
    disable_move: bool,
    extended_events: bool,
    clipboard_watch: Option<crate::clipboard::Subscription>,
    move_event: bool,
    viewport: krkr_protocol::viewport::Viewport,
    // Client size explicitly chosen by the script. Native user resizes fit
    // this canvas without depending on deprecated move/size-end callbacks.
    client_basis: Option<krkr_protocol::graphics::Size>,
    caption: Vec<u16>,
    visible: bool,
    stay_on_top: bool,
    // Windowed client extent captured before the host starts its asynchronous
    // fullscreen resize. Script zoom remains independent of presentation zoom.
    full_screen: Option<krkr_protocol::graphics::Size>,
    border_style: krkr_protocol::window::BorderStyle,
    min_size: (u32, u32),
    max_size: (u32, u32),
    focused: bool,
    closing: Option<Rc<Cell<bool>>>,
    user_closing: bool,
    pub(crate) cursor: (i32, i32),
    pub(crate) cursor_state: i32,
    pub(crate) default_ime: i32,
    pub(crate) input_style: Option<krkr_protocol::input_style::Style>,
    style_revision: u64,
    pending: VecDeque<Pending>,
    hint: Hints,
    recheck: Option<Duration>,
    recheck_queued: bool,
    menu: Option<ObjId>,
    invalidating: std::rc::Weak<()>,
}
pub(crate) struct Lease {
    shared: Shared,
    id: WindowId,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.borrow_mut().remove(self.id);
    }
}
impl Windows {
    pub(crate) fn owner(&self, id: WindowId) -> Option<ObjId> {
        self.records
            .get(id)
            .filter(|r| r.registered)
            .map(|r| r.owner)
    }
    pub(crate) fn graphics_window(&self) -> Option<(WindowId, ObjId)> {
        self.records
            .iter()
            .find(|(id, _)| self.is_live(*id))
            .map(|(id, r)| (id, r.owner))
    }
    /// Unregister before associated-object cleanup, as BaseWindow::Invalidate
    /// does. Keep the record/host lease until cleanup actually releases it.
    fn unregister(&mut self, id: WindowId) {
        self.finish_modal(id);
        let Some(record) = self.records.get_mut(id) else {
            return;
        };
        if !record.registered {
            return;
        }
        record.registered = false;
        record.clipboard_watch = None;
        if self.main != Some(id) {
            return;
        }
        self.main = None;
        if let Some(system) = self.system.upgrade() {
            system.borrow_mut().main_window_closed();
        }
    }
    pub fn closing(&self, heap: &Heap) -> bool {
        self.records
            .values()
            .any(|record| !record.registered && heap.is_finalizing(record.owner).unwrap_or(false))
    }
    fn remove(&mut self, id: WindowId) {
        self.unregister(id);
        if let Some(record) = self.records.remove(id) {
            if let Some(layers) = self.layers.upgrade() {
                layers.borrow_mut().forget_window(id);
            }
            record.alive.store(false, Ordering::Release);
            self.events.borrow_mut().remove(record.source);
            self.sources.remove(record.source);
            if self.main == Some(id) {
                self.main = None;
            }
            if let Some(host) = &self.host {
                host.wake_host();
            }
        }
    }
    pub fn reset(&mut self) {
        // Host reset/termination is cleanup, not a fresh script close event.
        self.main = None;
        for record in self.records.values_mut() {
            record.registered = false;
        }
        let ids: Vec<_> = self.records.keys().collect();
        for id in ids {
            self.remove(id);
        }
        if let Some(host) = &self.host {
            host.clear_events();
        }
    }
    fn record(&self, id: WindowId) -> NativeResult<&Record> {
        self.records
            .get(id)
            .ok_or(NativeError::Message("window is closed"))
    }
    fn record_mut(&mut self, id: WindowId) -> NativeResult<&mut Record> {
        self.records
            .get_mut(id)
            .ok_or(NativeError::Message("window is closed"))
    }
    fn create(shared: &Shared, owner: ObjId) -> NativeResult<Lease> {
        let mut world = shared.borrow_mut();
        let host = world.host.as_ref().ok_or(NativeError::Message(
            "Window requires a platform window host",
        ))?;
        if world.records.len() >= host.limits().windows {
            return Err(NativeError::Message("window capacity reached"));
        }
        let source = world.events.borrow_mut().insert(owner, Kind::Window, 1)?;
        let first = world.count() == 0;
        let id = world.records.insert(Record {
            registered: true,
            owner,
            source,
            alive: Arc::new(AtomicBool::new(true)),
            geometry: Geometry::default(),
            snapshot: None,
            icon: None,
            disable_resize: false,
            disable_move: false,
            extended_events: false,
            clipboard_watch: None,
            move_event: false,
            viewport: Default::default(),
            client_basis: None,
            caption: Vec::new(),
            visible: false,
            stay_on_top: false,
            full_screen: None,
            border_style: Default::default(),
            min_size: (0, 0),
            max_size: (0, 0),
            focused: false,
            closing: None,
            user_closing: false,
            cursor: (0, 0),
            cursor_state: 0,
            default_ime: 0,
            input_style: Some(Default::default()),
            style_revision: 0,
            pending: VecDeque::new(),
            hint: Hints::default(),
            recheck: None,
            recheck_queued: false,
            menu: None,
            invalidating: Default::default(),
        });
        world.sources.insert(source, id);
        if first {
            world.main = Some(id);
        }
        Ok(Lease {
            shared: shared.clone(),
            id,
        })
    }
    pub fn count(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.registered)
            .count()
    }
    pub fn has_resources(&self) -> bool {
        !self.records.is_empty()
    }
    pub(crate) fn contains(&self, id: WindowId) -> bool {
        self.records.contains_key(id)
    }
    pub(crate) fn is_live(&self, id: WindowId) -> bool {
        self.records
            .get(id)
            .is_some_and(|r| r.registered && r.invalidating.strong_count() == 0)
    }
    pub(crate) fn trace_input(&self, id: WindowId, visit: &mut dyn FnMut(Value)) {
        if let Some(layers) = self.layers.upgrade() {
            layers.borrow().trace_input(id, visit);
        }
    }
    pub fn pump(&mut self, budget: usize, system: &crate::system::Shared) {
        self.advance_input();
        self.pump_clipboard();
        let Some(host) = self.host.clone() else {
            return;
        };
        for _ in 0..budget {
            let Some(event) = host.peek_event() else {
                break;
            };
            if event.input.is_user_input() && !self.accepts_input(event.window) {
                host.pop_event();
                continue;
            }
            let activation = if let Input::Focus(focused) = event.input {
                let active = focused
                    || self
                        .records
                        .iter()
                        .any(|(id, r)| id != event.window && r.focused);
                let system = system.borrow();
                if active != system.active && system.activations.len() >= system.limit {
                    break;
                }
                Some(active)
            } else {
                None
            };
            let Some(record) = self.records.get_mut(event.window) else {
                host.pop_event();
                continue;
            };
            if !record.registered || record.invalidating.strong_count() != 0 {
                host.pop_event();
                continue;
            }
            // The windowEx provider opts a window into the portable move event.
            if (matches!(event.input, Input::Move(_)) && !record.move_event)
                || (matches!(event.input, Input::Extended(_)) && !record.extended_events)
            {
                if let Some(krkr_protocol::window::Event {
                    input: Input::Move(geometry),
                    ..
                }) = host.pop_event()
                {
                    let viewport_changed = record.geometry.inner_width != geometry.inner_width
                        || record.geometry.inner_height != geometry.inner_height;
                    record.geometry = geometry;
                    // Native move snapshots can already contain a new client
                    // size. Disabling the legacy callback must not suppress
                    // the canvas update or consume the later Resize's change.
                    if viewport_changed {
                        self.invalidate_viewport(event.window);
                    }
                }
                continue;
            }
            let replaces = matches!(record.pending.back(), Some(Pending::Input(previous))
                if event.input.replaces(*previous));
            // Keep one dispatch token per payload. In particular, internal
            // drawing waits pump host input faster than callbacks can run.
            // Preserve button/key boundaries while replacing stale motion.
            if !replaces && self.events.borrow_mut().post(record.source, 1, 0).is_err() {
                break;
            }
            let event = host.pop_event().expect("one input consumer");
            match event.input {
                Input::MouseMove { x, y, .. }
                | Input::MouseDown { x, y, .. }
                | Input::MouseUp { x, y, .. }
                | Input::Click { x, y }
                | Input::DoubleClick { x, y }
                | Input::Wheel { x, y, .. } => {
                    if record.cursor_state == 1 && record.cursor != (x, y) {
                        record.cursor_state = 0;
                    }
                    record.cursor = (x, y);
                    record.recheck.get_or_insert_with(|| {
                        self.clock.now().saturating_add(Duration::from_secs(1))
                    });
                }
                _ => {}
            }
            let mut viewport_changed = false;
            if let Input::Resize(geometry) | Input::Move(geometry) = event.input {
                viewport_changed = record.geometry.inner_width != geometry.inner_width
                    || record.geometry.inner_height != geometry.inner_height;
                record.geometry = geometry;
            }
            if let Input::Focus(focused) = event.input {
                record.focused = focused;
            }
            if replaces {
                *record.pending.back_mut().expect("pending input") = Pending::Input(event.input);
            } else {
                record.pending.push_back(Pending::Input(event.input));
            }
            if viewport_changed {
                self.invalidate_viewport(event.window);
            }
            if let Some(active) = activation {
                system.borrow_mut().set_active(active);
            }
        }
    }
    pub fn callback(
        &mut self,
        source: SourceId,
        heap: &mut Heap,
    ) -> Option<Box<dyn tjs_core::NativeContinuation>> {
        let id = *self.sources.get(source)?;
        let accepts_input = self.accepts_input(id);
        let record = self.records.get_mut(id)?;
        let input = match record.pending.pop_front()? {
            Pending::Suppressed => return None,
            Pending::ClipboardError(error) => {
                return Some(clipboard::failure(error));
            }
            Pending::Clipboard(token) => {
                return clipboard::callback(record.owner, token);
            }
            Pending::Transition(transition) => {
                return Some(crate::layer::transition::callback(
                    self.layers.upgrade()?,
                    transition,
                ));
            }
            Pending::Paint(layer) => {
                return Some(crate::layer::transition::paint_callback(
                    self.layers.upgrade()?,
                    layer,
                ));
            }
            Pending::Input(input) => input,
            Pending::Hint { text, position } => {
                if !accepts_input && !text.is_empty() {
                    return None;
                }
                return Some(presentation::hint_callback(
                    record.owner,
                    self.names[16],
                    text,
                    position,
                    heap,
                ));
            }
            Pending::Recheck => {
                record.recheck_queued = false;
                if !accepts_input {
                    return None;
                }
                return Some(crate::layer::recheck_input(
                    self.layers.upgrade()?,
                    id,
                    record.owner,
                ));
            }
        };
        if input.is_user_input() && !accepts_input {
            return None;
        }
        if matches!(input, Input::Close) {
            if record.user_closing || record.closing.is_some() {
                return None;
            }
            record.user_closing = true;
        }
        if matches!(input, Input::Move(_) | Input::Extended(_)) {
            return Some(extended::callback(heap, record.owner, input));
        }
        let (index, arguments) = callbacks::arguments(input, heap);
        Some(crate::layer::input_event(
            self.layers.upgrade()?,
            id,
            Value::Obj(tjs_core::ObjRef::bound(record.owner)),
            self.names[index],
            arguments,
            input,
        ))
    }
}
impl Trace for Windows {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.names.trace(visit);
        self.key_root.trace(visit);
        for key in self.post_keys {
            key.trace(visit);
        }
        // Idle records are weak; only queued callbacks keep their owners alive.
    }
}
pub(crate) fn install(
    heap: &mut Heap,
    operations: operations::Shared,
    events: events::Shared,
    clock: Rc<dyn tjs_runtime::clock::Clock>,
) -> NativeResult<Shared> {
    let names = callbacks::NAMES
        .iter()
        .map(|name| Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>())))
        .collect();
    let fields = [
        "type",
        "target",
        "x",
        "y",
        "button",
        "shift",
        "key",
        "delta",
        "layer",
        "blurred",
        "direction",
        "focused",
        "process",
        "dest",
        "src",
    ]
    .map(|name| heap.intern_str(name));
    // Symbols are collectible too. Retain cached field names in a private table
    // so every input callback can reuse them without allocating or re-interning.
    let key_root = heap.alloc_dictionary();
    for key in fields {
        heap.set_member(key_root, key, Value::Void)?;
    }
    let shared = Rc::new(RefCell::new(Windows {
        system: Default::default(),
        post_keys: ["key", "shift"]
            .map(|name| Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>()))),
        layers: Default::default(),
        host: None,
        operations,
        events,
        clock,
        records: SlotMap::with_key(),
        sources: SecondaryMap::new(),
        main: None,
        modals: Vec::new(),
        names,
        fields,
        key_root,
    }));
    bindings::install(heap, shared.clone())?;
    Ok(shared)
}
