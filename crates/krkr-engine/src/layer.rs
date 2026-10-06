//! VM-owned layer metadata. Pixel allocations and rendering stay in the host.
mod api;
mod bindings;
pub(crate) mod bitmap;
pub use bitmap::PixelContinuation;
pub(crate) use bitmap::{pixel_budget, read_pixels, write_pixels, write_shared_pixels};
mod cursor;
mod drawing;
pub(crate) mod extensions;
pub(crate) use extensions::{
    adjust as extension_adjust, perspective as extension_perspective,
    prepare_draw as extension_prepare_draw, size as extension_size,
    wrapped_copy as extension_wrapped_copy,
};
pub(crate) mod device;
pub(crate) mod gpu_image;
mod hit;
mod images;
mod input;
mod loading;
pub(crate) mod piled;
pub(crate) mod preload;
mod saving;
mod scene;
pub(crate) mod snapshot;
mod source;
mod tasks;
mod text;
mod transform;
pub(crate) mod transition;
mod tree;
pub(crate) mod update;
pub(crate) mod video;
pub(crate) use input::presentation::sync as sync_input;
pub(crate) use input::{event as input_event, recheck as recheck_input};
use krkr_protocol::{
    graphics::{Blend, DrawFace, ImageId, ImageRef, LayerId, Node, Rect, Scene, Size},
    window::{Client, WindowId},
};
use slotmap::SlotMap;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};
pub(crate) use text::{font_link, font_require_main, font_settings, update_font};
use tjs_core::{Heap, NativeError, NativeResult, ObjId, ObjRef, Trace, Value};

pub(crate) type Shared = Rc<RefCell<Layers>>;
pub(crate) struct Layers {
    windows: crate::window::Shared,
    records: SlotMap<LayerId, Record>,
    images: SlotMap<ImageId, ()>,
    primary: HashMap<WindowId, LayerId>,
    movies: HashMap<ImageId, video::Plane>,
    device_frames: HashMap<WindowId, device::Frame>,
    device_input: HashMap<WindowId, usize>,
    creation_order: u64,
    movie_order: u64,
    dirty: HashSet<WindowId>,
    paint_finished: HashSet<WindowId>,
    paint_active: HashSet<WindowId>,
    hit_cache: hit::Cache,
    input: HashMap<WindowId, input::State>,
    cursors: cursor::Cache,
    names: HashMap<&'static str, Value>,
    transitions: SlotMap<transition::Id, transition::Active>,
    transition_providers: HashMap<&'static str, Arc<transition::provider::Provider>>,
    pub failure: Option<String>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    image_left: i32,
    image_top: i32,
    image_size: Size,
    clip: Rect,
}
impl Geometry {
    // TJS dimensions are signed. Offscreen animation may temporarily invert
    // an extent; keep it visible to scripts but submit an empty drawing area.
    fn size(&self) -> Size {
        Size {
            width: self.width.max(0) as u32,
            height: self.height.max(0) as u32,
        }
    }
    fn set_size(&mut self, size: Size) {
        self.width = size.width as i32;
        self.height = size.height as i32;
    }
}
struct Record {
    creation_order: u64,
    owner: ObjId,
    action_owner: Value,
    window: WindowId,
    root: LayerId,
    parent: Option<LayerId>,
    children: Vec<LayerId>,
    children_array: Option<ObjId>,
    children_dirty: bool,
    absolute_order: i32,
    absolute_order_mode: bool,
    image: Option<ImageRef>,
    has_main: bool,
    image_modified: bool,
    cache: Option<Arc<()>>,
    geometry: Geometry,
    visible: bool,
    ready: bool,
    opacity: u8,
    blend: Blend,
    neutral: u32,
    face: i32,
    hold_alpha: bool,
    name: Vec<u16>,
    font: krkr_protocol::text::Font,
    font_object: Option<ObjId>,
    enabled: bool,
    enabled_work: bool,
    hit_type: i32,
    hit_threshold: i32,
    hit_work: bool,
    focusable: bool,
    join_focus_chain: bool,
    focus_work: Option<LayerId>,
    shutdown: bool,
    cursor: i32,
    cursor_x: i32,
    hint: Arc<[u16]>,
    show_parent_hint: bool,
    ignore_hint_sensing: bool,
    ime: i32,
    attention: (i32, i32),
    use_attention: bool,
    transition: Option<transition::Id>,
    call_on_paint: bool,
    paint_queued: bool,
}
impl Record {
    fn image(&self) -> NativeResult<&ImageRef> {
        self.image
            .as_ref()
            .filter(|_| self.has_main)
            .ok_or(NativeError::Message("layer has no drawable image"))
    }
    fn face(&self) -> NativeResult<DrawFace> {
        Ok(match self.face {
            0 => DrawFace::Alpha,
            1 => DrawFace::Opaque,
            2 => DrawFace::Mask,
            3 => DrawFace::Province,
            4 => DrawFace::AddAlpha,
            128 => self.blend.face(),
            _ => return Err(NativeError::Message("invalid draw face")),
        })
    }
    fn neutral(&self) -> u32 {
        self.neutral
    }
}
struct Lease {
    shared: Shared,
    id: LayerId,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.borrow_mut().remove(self.id);
    }
}
impl Layers {
    fn record(&self, id: LayerId) -> NativeResult<&Record> {
        self.records
            .get(id)
            .ok_or(NativeError::Message("layer is invalid"))
    }
    fn record_mut(&mut self, id: LayerId) -> NativeResult<&mut Record> {
        self.records
            .get_mut(id)
            .ok_or(NativeError::Message("layer is invalid"))
    }
    fn host(&self) -> NativeResult<Client> {
        self.windows
            .borrow()
            .host
            .clone()
            .ok_or(NativeError::Message(
                "Layer requires a platform graphics host",
            ))
    }
    fn create(
        shared: &Shared,
        owner: ObjId,
        action_owner: Value,
        window: WindowId,
        parent: Option<LayerId>,
    ) -> NativeResult<Lease> {
        let mut world = shared.borrow_mut();
        // Presentation admission is per window. Independent windows must not
        // consume one another's scene quota; the host also bounds window count.
        if world
            .records
            .values()
            .filter(|r| r.window == window)
            .count()
            >= world.host()?.limits().scene_nodes
        {
            return Err(NativeError::Message("layer capacity reached"));
        }
        if let Some(parent) = parent {
            if world.record(parent)?.window != window {
                return Err(NativeError::Message(
                    "parent belongs to another layer tree owner",
                ));
            }
            let mut depth = 0;
            let mut ancestor = Some(parent);
            while let Some(id) = ancestor {
                depth += 1;
                ancestor = world.record(id)?.parent;
            }
            if depth > 128 {
                return Err(NativeError::Message(
                    "layer nesting exceeds the renderer limit",
                ));
            }
        } else if world.primary.contains_key(&window) {
            return Err(NativeError::Message("window already has a primary layer"));
        }
        let image = ImageRef {
            id: world.images.insert(()),
            lifetime: Arc::default(),
        };
        let size = Size {
            width: 32,
            height: 32,
        };
        let root = parent.map(|id| world.records[id].root);
        let creation_order = world.creation_order;
        world.creation_order = creation_order
            .checked_add(1)
            .ok_or(NativeError::Message("layer order exhausted"))?;
        let id = world.records.insert_with_key(|id| Record {
            creation_order,
            owner,
            action_owner,
            window,
            root: root.unwrap_or(id),
            parent,
            children: Vec::new(),
            children_array: None,
            children_dirty: true,
            absolute_order: 0,
            absolute_order_mode: false,
            image: Some(image),
            has_main: true,
            image_modified: false,
            geometry: Geometry {
                left: 0,
                top: 0,
                width: size.width as i32,
                height: size.height as i32,
                image_left: 0,
                image_top: 0,
                image_size: size,
                clip: size.rect(),
            },
            cache: None,
            visible: parent.is_none(),
            ready: false,
            opacity: 255,
            blend: if parent.is_none() {
                Blend::Opaque
            } else {
                Blend::Alpha
            },
            neutral: if parent.is_none() {
                0xffffffff
            } else {
                0x00ffffff
            },
            face: 128,
            hold_alpha: false,
            name: Vec::new(),
            font: Default::default(),
            font_object: None,
            enabled: true,
            enabled_work: true,
            hit_type: 0,
            hit_threshold: if parent.is_none() { 0 } else { 16 },
            hit_work: true,
            focusable: false,
            join_focus_chain: true,
            focus_work: None,
            shutdown: false,
            cursor: 0,
            cursor_x: 0,
            hint: Arc::default(),
            show_parent_hint: true,
            ignore_hint_sensing: false,
            ime: 0,
            attention: (0, 0),
            use_attention: false,
            transition: None,
            call_on_paint: false,
            paint_queued: false,
        });
        if let Some(parent) = parent {
            world.records[parent].children.push(id);
            world.records[parent].children_dirty = true;
            world.joined_order(id, parent);
        } else {
            world.primary.insert(window, id);
        }
        world.changed(window);
        Ok(Lease {
            shared: shared.clone(),
            id,
        })
    }
    fn remove(&mut self, id: LayerId) {
        self.remove_input_layer(id);
        if let Some(record) = self.records.remove(id) {
            if let Some(parent) = record.parent.and_then(|id| self.records.get_mut(id)) {
                parent.children.retain(|&child| child != id);
                parent.children_dirty = true;
            }
            for child in record.children {
                if let Some(record) = self.records.get_mut(child) {
                    record.parent = None;
                }
            }
            if self.primary.get(&record.window) == Some(&id) {
                self.primary.remove(&record.window);
            }
            if let Some(image) = record.image {
                self.hit_cache.remove(image.id);
                self.images.remove(image.id);
            }
            self.changed(record.window);
        }
    }
    pub fn reset(&mut self) {
        self.movies.clear();
        self.transitions.clear();
        self.input.clear();
        self.cursors = cursor::Cache::default();
        self.hit_cache = hit::Cache::default();
        self.records.clear();
        self.images.clear();
        self.primary.clear();
        self.dirty.clear();
        self.paint_finished.clear();
        self.paint_active.clear();
        self.failure = None;
    }
    pub fn primary(&self, window: WindowId) -> NativeResult<Value> {
        self.primary
            .get(&window)
            .and_then(|id| self.records.get(*id))
            .map(|r| object(r.owner))
            .ok_or(NativeError::Message("window has no primary layer"))
    }
    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }
}
fn null() -> Value {
    Value::Obj(ObjRef {
        object: None,
        this: None,
    })
}
pub(crate) fn focused_layer(shared: &Shared, window: WindowId) -> Value {
    let world = shared.borrow();
    world
        .focused(window)
        .and_then(|id| world.records.get(id))
        .map(|r| object(r.owner))
        .unwrap_or_else(null)
}
pub(crate) fn set_focused_layer(
    shared: Shared,
    window: WindowId,
    heap: &mut Heap,
    value: Value,
) -> NativeResult<tjs_core::NativeStep> {
    let target = match value {
        Value::Void | Value::Obj(ObjRef { object: None, .. }) => None,
        _ => Some(bindings::layer_id(heap, value)?),
    };
    if let Some(target) = target
        && shared.borrow().record(target)?.window != window
    {
        return Err(NativeError::Message(
            "focused layer belongs to another window",
        ));
    }
    Ok(input::focus::set(
        shared,
        window,
        target,
        true,
        Box::new(input::Returned),
    ))
}
fn object(id: ObjId) -> Value {
    Value::Obj(ObjRef::bound(id))
}
fn object_id(value: Value) -> NativeResult<ObjId> {
    if let Value::Obj(object) = value {
        object
            .object
            .ok_or(NativeError::Message("object must not be null"))
    } else {
        Err(NativeError::Message("object is required"))
    }
}
pub(crate) fn install(heap: &mut Heap, windows: crate::window::Shared) -> NativeResult<Shared> {
    let names = input::NAMES
        .iter()
        .map(|&name| {
            (
                name,
                Value::Str(heap.alloc_string(name.encode_utf16().collect::<Vec<_>>())),
            )
        })
        .collect();
    let world = Rc::new(RefCell::new(Layers {
        windows: windows.clone(),
        records: SlotMap::with_key(),
        images: SlotMap::with_key(),
        primary: HashMap::new(),
        movies: HashMap::new(),
        device_frames: HashMap::new(),
        device_input: HashMap::new(),
        creation_order: 0,
        movie_order: 0,
        dirty: HashSet::new(),
        paint_finished: HashSet::new(),
        paint_active: HashSet::new(),
        hit_cache: hit::Cache::default(),
        input: HashMap::new(),
        cursors: cursor::Cache::default(),
        names,
        transitions: SlotMap::with_key(),
        transition_providers: HashMap::new(),
        failure: None,
    }));
    windows.borrow_mut().layers = Rc::downgrade(&world);
    bindings::install(heap, world.clone())?;
    Ok(world)
}
