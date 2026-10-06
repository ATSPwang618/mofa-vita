pub mod desktop;
mod draws;
use slotmap::new_key_type;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

new_key_type! { pub struct WindowId; }
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Physical pixels on the display hosting the main window (or the default
/// display before window creation). Desktop bounds may exclude system panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    pub width: u32,
    pub height: u32,
    pub desktop_left: i32,
    pub desktop_top: i32,
    pub desktop_width: u32,
    pub desktop_height: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(i32)]
pub enum BorderStyle {
    None = 0,
    Single = 1,
    #[default]
    Sizeable = 2,
    Dialog = 3,
    ToolWindow = 4,
    SizeToolWin = 5,
}
impl TryFrom<i32> for BorderStyle {
    type Error = &'static str;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::None,
            1 => Self::Single,
            2 => Self::Sizeable,
            3 => Self::Dialog,
            4 => Self::ToolWindow,
            5 => Self::SizeToolWin,
            _ => return Err("invalid window border style"),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Geometry {
    pub left: i32,
    pub top: i32,
    pub client_left: i32,
    pub client_top: i32,
    pub width: u32,
    pub height: u32,
    pub inner_width: u32,
    pub inner_height: u32,
}

/// Screen coordinates and physical pixels; never a native window handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rectangle {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub geometry: Geometry,
    pub visible: bool,
    pub outer: Option<Rectangle>,
    pub client: Option<Rectangle>,
    pub normal: Option<Rectangle>,
    /// Some backends cannot expose placement relative to the desktop work area.
    pub normal_workspace: Option<Rectangle>,
    pub maximized: bool,
    pub minimized: Option<bool>,
    pub maximize_box: Option<bool>,
    pub minimize_box: Option<bool>,
}

#[derive(Clone, Copy, Debug)]
pub enum Control {
    Minimize,
    Maximize,
    Restore,
    Maximized(bool),
    Minimized(bool),
}

#[derive(Clone, Copy, Debug)]
pub enum ZOrder {
    Top,
    Bottom,
    Topmost,
    NotTopmost,
    Behind(WindowId),
}

#[derive(Debug)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub rgba: crate::pixels::Bytes,
}

/// None restores the backend's executable/default icon. Application icons
/// belong to the host session and may be set before any windows exist.
#[derive(Debug)]
pub enum IconCommand {
    Window {
        image: Option<Arc<IconImage>>,
        with_application: bool,
    },
    Application(Option<Arc<IconImage>>),
}

/// Directory chooser inputs use native path text, without handles or script objects.
#[derive(Clone, Debug, Default)]
pub struct DirectoryDialog {
    /// An explicitly present but empty owner falls back to the application.
    pub application_owner: bool,
    pub title: Vec<u16>,
    pub initial: Vec<u16>,
    pub root: Vec<u16>,
}

#[derive(Debug)]
pub enum Command {
    Inform {
        text: String,
        caption: String,
        buttons: Vec<String>,
    },
    SelectDirectory(DirectoryDialog),
    Desktop(desktop::Command),
    Icon(IconCommand),
    Graphics(crate::graphics::Command),
    Create {
        alive: Weak<AtomicBool>,
        caption: String,
    },
    Caption(String),
    Visible(bool),
    /// Show a script-drawn modal window. Complete only after `active` is
    /// cleared/dropped and presentation/input ownership have been restored.
    /// Single-screen hosts can compose this logical window above its parent.
    ShowModal {
        active: Weak<AtomicBool>,
    },
    Position(i32, i32),
    Size {
        width: u32,
        height: u32,
        inner: bool,
    },
    MinSize(u32, u32),
    MaxSize(u32, u32),
    Focus,
    StayOnTop(bool),
    FullScreen(bool),
    BorderStyle(BorderStyle),
    BeginMove,
    QueryState,
    Control(Control),
    MaximizeBox(bool),
    MinimizeBox(bool),
    DisableResize(bool),
    DisableMove(bool),
    ZOrder {
        order: ZOrder,
        activate: bool,
    },
    HideCursor,
    InputStyle(crate::input_style::Style),
    CursorState(i32),
    CursorPosition(i32, i32),
    RegisterCursor {
        id: i32,
        image: Arc<crate::input_style::CursorImage>,
    },
}
impl Command {
    fn text_bytes(&self) -> usize {
        match self {
            Self::Inform {
                text,
                caption,
                buttons,
            } => text
                .len()
                .saturating_add(caption.len())
                .saturating_add(buttons.iter().map(String::len).sum::<usize>()),
            Self::Create { caption, .. } | Self::Caption(caption) => caption.len(),
            Self::SelectDirectory(dialog) => {
                (dialog.title.len() + dialog.initial.len() + dialog.root.len()).saturating_mul(2)
            }
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExtendedEvent {
    Minimize,
    Maximize,
    Show,
    Hide,
    DpiChanged(u32, u32),
    DisplayChanged,
    ActivateChanged(bool, Option<bool>),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Extended(ExtendedEvent),
    Resize(Geometry),
    Move(Geometry),
    Focus(bool),
    Close,
    MouseEnter,
    MouseLeave,
    MouseMove {
        x: i32,
        y: i32,
        shift: u32,
    },
    MouseDown {
        x: i32,
        y: i32,
        button: i32,
        shift: u32,
    },
    MouseUp {
        x: i32,
        y: i32,
        button: i32,
        shift: u32,
    },
    Click {
        x: i32,
        y: i32,
    },
    DoubleClick {
        x: i32,
        y: i32,
    },
    Wheel {
        shift: u32,
        delta: i32,
        x: i32,
        y: i32,
    },
    KeyDown {
        key: u32,
        shift: u32,
    },
    KeyUp {
        key: u32,
        shift: u32,
    },
    KeyPress(u16),
}
impl Input {
    /// User interaction that a window modal scope must block in other windows.
    /// Geometry and activation notifications remain deliverable.
    pub fn is_user_input(self) -> bool {
        !matches!(
            self,
            Self::Resize(_) | Self::Move(_) | Self::Focus(_) | Self::Extended(_)
        )
    }
    /// Whether this input can replace an adjacent, undispatched input for the
    /// same window. Both host and engine queues must coalesce: draining one
    /// queue into the other must not preserve an obsolete pointer trajectory.
    pub fn replaces(self, older: Self) -> bool {
        match (self, older) {
            (
                Self::MouseMove { shift, .. },
                Self::MouseMove {
                    shift: previous, ..
                },
            ) => shift == previous,
            (Self::Resize(_), Self::Resize(_)) | (Self::Move(_), Self::Move(_)) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub window: WindowId,
    pub input: Input,
}

/// Limits include requests executing on the host and replies awaiting consumption.
/// Input payloads are fixed size; captions have a separate per-request byte bound.
#[derive(Clone, Copy)]
pub struct Limits {
    pub windows: usize,
    pub operations: usize,
    pub events: usize,
    pub caption_bytes: usize,
    pub graphics_bytes: usize,
    /// Shared CPU image decode/upload/readback byte allowance.
    pub staging_bytes: usize,
    /// Evictable references within the GPU resident pool, not another pool.
    pub image_cache_bytes: usize,
    /// Maximum nodes in one window's scene, including its transition sources.
    pub scene_nodes: usize,
    pub hit_cache_bytes: usize,
    pub cursors: usize,
    pub cursor_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            windows: 64,
            operations: 64,
            events: 1024,
            caption_bytes: 64 * 1024,
            graphics_bytes: 256 * 1024,
            staging_bytes: 32 * 1024 * 1024,
            image_cache_bytes: 32 * 1024 * 1024,
            scene_nodes: 1024,
            hit_cache_bytes: 8 * 1024 * 1024,
            cursors: 128,
            cursor_bytes: 4 * 1024 * 1024,
        }
    }
}

struct Shared {
    draws: Mutex<Option<draws::Grant>>,
    requests: Mutex<VecDeque<Request>>,
    events: Mutex<VecDeque<Event>>,
    failure: Mutex<Option<String>>,
    active: AtomicUsize,
    connected: AtomicBool,
    limits: Limits,
    host_wake: Wake,
    client_wake: Mutex<Wake>,
    scenes: Mutex<HashMap<WindowId, crate::graphics::Scene>>,
    sequence: std::sync::atomic::AtomicU64,
    staging: crate::budget::Budget,
    image_cache: crate::image_cache::Cache,
    keys: crate::keys::Keys,
    display: Mutex<Option<Display>>,
    transition_kernels: Mutex<std::collections::HashSet<String>>,
}
#[derive(Clone)]
pub struct Client(Arc<Shared>);
pub struct Host(Arc<Shared>);
struct Reply {
    admitted: bool,
    shared: Arc<Shared>,
    active: AtomicBool,
    result: Mutex<Option<Result<Response, String>>>,
}
#[derive(Debug)]
pub enum Response {
    DirectorySelected(Option<Vec<u16>>),
    Informed(i32),
    Desktop(desktop::Response),
    Geometry(Geometry),
    Snapshot(Snapshot),
    Done,
    /// Asset upload completed; actual retained storage after backend conversion.
    ImageStorage(usize),
    Pixel(u32),
    Image(crate::pixels::Pixels),
    HitPlane(crate::hit::Plane),
}
impl Drop for Reply {
    fn drop(&mut self) {
        self.shared.active.fetch_sub(1, Ordering::Relaxed);
    }
}
pub struct Ticket(Arc<Reply>);
impl Ticket {
    pub fn take(&self) -> Option<Result<Response, String>> {
        let result = self
            .0
            .result
            .lock()
            .unwrap()
            .take()
            .or_else(|| self.0.shared.failure.lock().unwrap().clone().map(Err));
        if result.is_some() {
            self.0.active.store(false, Ordering::Release);
        }
        result
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if self.0.active.swap(false, Ordering::AcqRel) {
            (self.0.shared.host_wake)();
        }
    }
}
pub struct Request {
    pub window: WindowId,
    pub command: Command,
    pub sequence: u64,
    reply: Arc<Reply>,
}
/// Scene commits are consumed before any later image writes. A host must
/// retain their image versions before asking for the next update.
pub enum Update {
    Request(Request),
    Scenes(Vec<(WindowId, crate::graphics::Scene)>),
}
impl Request {
    pub fn cancelled(&self) -> bool {
        !self.reply.active.load(Ordering::Acquire)
    }
    pub fn complete(self, result: Result<Geometry, String>) {
        self.respond(result.map(Response::Geometry));
    }
    pub fn respond(self, result: Result<Response, String>) {
        if self.reply.admitted {
            if let Err(error) = result {
                self.reply.shared.fail(error);
            }
            return;
        }
        *self.reply.result.lock().unwrap() = Some(result);
        self.reply.shared.wake_client();
    }
}
impl Shared {
    fn wake_client(&self) {
        let wake = self.client_wake.lock().unwrap().clone();
        wake();
    }
    fn fail(&self, error: String) {
        self.connected.store(false, Ordering::Release);
        self.failure.lock().unwrap().get_or_insert(error);
        // Break Shared -> Request -> Reply -> Shared on disconnect.
        self.requests.lock().unwrap().clear();
        self.draws.lock().unwrap().take();
        self.scenes.lock().unwrap().clear();
        self.wake_client();
    }
}
pub fn channel(limits: Limits, host_wake: Wake) -> (Client, Host) {
    let shared = Arc::new(Shared {
        draws: Mutex::new(None),
        requests: Mutex::new(VecDeque::new()),
        events: Mutex::new(VecDeque::new()),
        failure: Mutex::new(None),
        active: AtomicUsize::new(0),
        connected: AtomicBool::new(true),
        limits,
        host_wake,
        client_wake: Mutex::new(Arc::new(|| {})),
        scenes: Mutex::new(HashMap::new()),
        sequence: std::sync::atomic::AtomicU64::new(0),
        staging: crate::budget::Budget::new(limits.staging_bytes),
        image_cache: crate::image_cache::Cache::new(limits.image_cache_bytes),
        keys: Default::default(),
        display: Mutex::new(None),
        transition_kernels: Mutex::new(Default::default()),
    });
    (Client(shared.clone()), Host(shared))
}
impl Client {
    pub fn supports_transition(&self, kernel: &str) -> bool {
        self.0.transition_kernels.lock().unwrap().contains(kernel)
    }
    pub fn image_cache(&self) -> crate::image_cache::Cache {
        self.0.image_cache.clone()
    }
    pub fn display(&self) -> Result<Display, String> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        self.0
            .display
            .lock()
            .unwrap()
            .ok_or_else(|| "display information is unavailable".into())
    }
    pub fn key_state(&self, key: u32, current: bool) -> bool {
        self.0.keys.get(key, current)
    }
    pub fn staging_budget(&self) -> crate::budget::Budget {
        self.0.staging.clone()
    }
    pub fn limits(&self) -> Limits {
        self.0.limits
    }
    pub fn set_waker(&self, wake: Wake) {
        *self.0.client_wake.lock().unwrap() = wake;
    }
    pub fn wake_host(&self) {
        (self.0.host_wake)();
    }
    pub fn failure(&self) -> Option<String> {
        self.0.failure.lock().unwrap().clone()
    }
    pub fn request(&self, window: WindowId, command: Command) -> Result<Ticket, String> {
        if let Command::Graphics(command) = &command
            && command.payload_bytes() > self.0.limits.graphics_bytes
        {
            return Err("graphics operation exceeds the host byte limit".into());
        }
        if command.text_bytes() > self.0.limits.caption_bytes {
            return Err("window caption exceeds the host byte limit".into());
        }
        let mut requests = self.0.requests.lock().unwrap();
        if !self.0.connected.load(Ordering::Acquire) {
            return Err("window host disconnected".into());
        }
        // Queue previously admitted writes even if this next operation cannot
        // get a slot. Empty optional grants release their slot here too.
        self.0.flush_draws(&mut requests);
        if self
            .0
            .active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < self.0.limits.operations).then(|| count + 1)
            })
            .is_err()
        {
            drop(requests);
            self.wake_host();
            return Err("window operation capacity reached".into());
        }
        let reply = Arc::new(Reply {
            admitted: false,
            shared: self.0.clone(),
            active: AtomicBool::new(true),
            result: Mutex::new(None),
        });
        if let Command::Graphics(command) = &command {
            command.invalidate_snapshots();
        }
        requests.push_back(Request {
            window,
            command,
            reply: reply.clone(),
            sequence: self.0.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        });
        drop(requests);
        self.wake_host();
        Ok(Ticket(reply))
    }
    pub fn peek_event(&self) -> Option<Event> {
        self.0.events.lock().unwrap().front().copied()
    }
    pub fn pop_event(&self) -> Option<Event> {
        self.0.events.lock().unwrap().pop_front()
    }
    pub fn clear_events(&self) {
        self.0.events.lock().unwrap().clear();
    }
    pub fn clear_user_input(&self) {
        self.0
            .events
            .lock()
            .unwrap()
            .retain(|event| !event.input.is_user_input());
        self.0.keys.release();
    }
    pub fn publish(
        &self,
        window: WindowId,
        mut scene: crate::graphics::Scene,
    ) -> Result<(), String> {
        if scene.nodes.capacity() > self.0.limits.scene_nodes
            || scene.transitions.capacity() > self.0.limits.scene_nodes
        {
            return Err("scene node capacity reached".into());
        }
        let mut ordered = self.0.requests.lock().unwrap();
        if !self.0.connected.load(Ordering::Acquire) {
            return Err("window host disconnected".into());
        }
        self.0.flush_draws(&mut ordered);
        let mut scenes = self.0.scenes.lock().unwrap();
        if !scenes.contains_key(&window) && scenes.len() >= self.0.limits.windows {
            return Err("scene window capacity reached".into());
        }
        scene.requires_op_seq = self.0.sequence.load(Ordering::Relaxed);
        scenes.insert(window, scene);
        drop(scenes);
        drop(ordered);
        self.wake_host();
        Ok(())
    }
}
impl Host {
    pub fn next_update(&self, completed_sequence: u64) -> Option<Update> {
        // Same lock order as Client::publish: a later write cannot slip between
        // checking the scene fence and removing the next request.
        let mut requests = self.0.requests.lock().unwrap();
        let mut scenes = self.0.scenes.lock().unwrap();
        let ready: Vec<_> = scenes
            .extract_if(|_, scene| scene.requires_op_seq <= completed_sequence)
            .collect();
        if !ready.is_empty() {
            return Some(Update::Scenes(ready));
        }
        requests.pop_front().map(Update::Request)
    }
    /// Publish identities backed by this host's installed render kernels.
    pub fn set_transition_kernels(&self, kernels: impl IntoIterator<Item = String>) {
        *self.0.transition_kernels.lock().unwrap() = kernels.into_iter().collect();
    }
    pub fn image_cache(&self) -> crate::image_cache::Cache {
        self.0.image_cache.clone()
    }
    pub fn set_display(&self, display: Option<Display>) {
        *self.0.display.lock().unwrap() = display;
    }
    pub fn staging_budget(&self) -> crate::budget::Budget {
        self.0.staging.clone()
    }
    pub fn limits(&self) -> Limits {
        self.0.limits
    }
    pub fn next_request(&self) -> Option<Request> {
        self.0.requests.lock().unwrap().pop_front()
    }
    pub fn take_scenes(&self, completed_sequence: u64) -> Vec<(WindowId, crate::graphics::Scene)> {
        self.0
            .scenes
            .lock()
            .unwrap()
            .extract_if(|_, scene| scene.requires_op_seq <= completed_sequence)
            .collect()
    }
    /// Supplement script key codes with the host's left/right physical identity.
    pub fn update_key_state(&self, key: u32, pressed: bool) {
        self.0.keys.update(key, pressed);
    }
    /// Coalesce only adjacent motion/geometry messages. Keys and close requests
    /// remain ordered; exhaustion is a reported session failure, never silent loss.
    pub fn post(&self, event: Event) -> Result<(), String> {
        match event.input {
            Input::KeyDown { key, .. } => self.0.keys.update(key, true),
            Input::KeyUp { key, .. } => {
                let held = (16..=18).contains(&key)
                    && (self.0.keys.held(160 + (key - 16) * 2)
                        || self.0.keys.held(161 + (key - 16) * 2));
                self.0.keys.update(key, held);
            }
            Input::MouseDown { button, .. } | Input::MouseUp { button, .. } => {
                if let Some(&key) = [1, 2, 4, 5, 6].get(button as usize) {
                    self.0
                        .keys
                        .update(key, matches!(event.input, Input::MouseDown { .. }));
                }
            }
            Input::Focus(false) => self.0.keys.release(),
            _ => {}
        }
        let mut events = self.0.events.lock().unwrap();
        if let Some(last) = events.back_mut()
            && last.window == event.window
            && event.input.replaces(last.input)
        {
            *last = event;
            return Ok(());
        }
        if events.len() >= self.0.limits.events {
            drop(events);
            let error = "window input capacity reached".to_string();
            self.0.fail(error.clone());
            return Err(error);
        }
        events.push_back(event);
        drop(events);
        self.0.wake_client();
        Ok(())
    }
    pub fn disconnect(&self, error: String) {
        self.0.fail(error);
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.0.fail("window host disconnected".into());
    }
}
