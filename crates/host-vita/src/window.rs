//! Single-display presentation of script windows. The VM never owns EGL objects.
use crate::graphics::{Graphics, Snapshot};
use krkr_protocol::{
    graphics::{Rect, Size},
    viewport::Viewport,
    window::{self, Command, Geometry, Host, Input, Request, Response, Update, WindowId},
};
use krkr_render_gles2::{Gpu, Image, SceneState};
use std::{
    collections::HashMap,
    sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
};

pub const DISPLAY: Size = Size {
    width: 960,
    height: 544,
};
#[cfg(all(test, target_os = "linux"))]
#[allow(unsafe_code)]
#[path = "../tests/window/internal.rs"]
mod tests;
fn live(lease: &Weak<AtomicBool>) -> bool {
    lease
        .upgrade()
        .is_some_and(|flag| flag.load(Ordering::Acquire))
}
fn geometry(size: Size, left: i32, top: i32) -> Geometry {
    Geometry {
        left,
        top,
        client_left: left,
        client_top: top,
        width: size.width,
        height: size.height,
        inner_width: size.width,
        inner_height: size.height,
    }
}
fn size(g: Geometry) -> Size {
    Size {
        width: g.inner_width.max(1),
        height: g.inner_height.max(1),
    }
}
fn rectangle(g: Geometry) -> window::Rectangle {
    window::Rectangle {
        x: g.left,
        y: g.top,
        width: g.width,
        height: g.height,
    }
}

/// A shared rounded viewport maps both display and input. Outside the client
/// remains outside: letterbox touches must not activate an edge button.
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    pub logical: Size,
    pub viewport: Viewport,
}
impl Mapping {
    pub fn fit(logical: Size) -> Self {
        let (n, d) = if u64::from(DISPLAY.width) * u64::from(logical.height)
            <= u64::from(DISPLAY.height) * u64::from(logical.width)
        {
            (DISPLAY.width, logical.width)
        } else {
            (DISPLAY.height, logical.height)
        };
        let mut viewport = Viewport::default()
            .zoom(n as i32, d.max(1) as i32)
            .expect("positive display ratio");
        let dest = viewport.destination(logical);
        viewport.left = ((DISPLAY.width - dest.width) / 2) as i32;
        viewport.top = ((DISPLAY.height - dest.height) / 2) as i32;
        Self { logical, viewport }
    }
    pub fn destination(self) -> Rect {
        self.viewport.destination(self.logical)
    }
    pub fn input(self, point: (i32, i32)) -> (i32, i32) {
        let dest = self.destination();
        let axis = |position: i32, origin: i32, logical: u32, physical: u32| {
            ((i64::from(position) - i64::from(origin)) * i64::from(logical))
                .div_euclid(i64::from(physical))
                .clamp(i32::MIN as i64, i32::MAX as i64) as i32
        };
        (
            axis(point.0, dest.left, self.logical.width, dest.width),
            axis(point.1, dest.top, self.logical.height, dest.height),
        )
    }
    pub fn output(self, point: (i32, i32)) -> (i32, i32) {
        self.viewport.to_window(self.logical, point)
    }
    fn map_rect(self, rect: Rect) -> Rect {
        self.viewport.attention(self.logical, rect)
    }
}
struct Window {
    alive: Weak<AtomicBool>,
    geometry: Geometry,
    normal: Geometry,
    visible: bool,
    minimized: bool,
    maximized: bool,
    fullscreen: bool,
    topmost: bool,
    maximize_box: bool,
    minimize_box: bool,
    cursor_state: i32,
    style: krkr_protocol::input_style::Style,
    snapshot: Option<Snapshot>,
    canvas: Option<Image>,
    scene_state: SceneState,
    viewport: Viewport,
}
impl Window {
    fn surface_sizes(
        &self,
        scene: &krkr_protocol::graphics::Scene,
        mapping: Mapping,
        gpu: &Gpu,
    ) -> (Size, Size) {
        let logical = if scene.viewport != Viewport::default() {
            scene
                .nodes
                .iter()
                .find(|n| n.parent.is_none() && n.visible)
                .map(|n| Size {
                    width: n.rectangle.width,
                    height: n.rectangle.height,
                })
                .unwrap_or_else(|| size(self.geometry))
        } else {
            size(self.geometry)
        };
        let destination = mapping.map_rect(scene.viewport.destination(logical));
        let physical = Size {
            width: destination.width.min(DISPLAY.width).min(logical.width),
            height: destination.height.min(DISPLAY.height).min(logical.height),
        };
        (logical, gpu.scene_storage_size(logical, physical))
    }
}
struct Modal {
    request: Request,
    active: Weak<AtomicBool>,
    restore: Option<WindowId>,
}
pub struct Windows {
    pub host: Host,
    pub graphics: Graphics,
    windows: HashMap<WindowId, Window>,
    order: Vec<WindowId>,
    primary: Option<WindowId>,
    focused: Option<WindowId>,
    modals: Vec<Modal>,
    completed: u64,
    dirty: bool,
    presented_scene: bool,
    cursor: (i32, i32),
    cursors: crate::cursor::Cursors,
    pointer_visible: bool,
    overlay: Option<crate::overlay::Overlay>,
    pub(crate) input_panel: Option<crate::input::panel::Panel>,
}
impl Windows {
    pub fn new(host: Host, gpu: Gpu) -> Self {
        host.set_display(Some(window::Display {
            width: DISPLAY.width,
            height: DISPLAY.height,
            desktop_left: 0,
            desktop_top: 0,
            desktop_width: DISPLAY.width,
            desktop_height: DISPLAY.height,
        }));
        host.set_transition_kernels(gpu.transition_kernels());
        let graphics = Graphics::new(gpu, host.image_cache());
        Self {
            host,
            graphics,
            windows: HashMap::new(),
            order: Vec::new(),
            primary: None,
            focused: None,
            modals: Vec::new(),
            completed: 0,
            dirty: true,
            presented_scene: false,
            cursor: (480, 272),
            cursors: Default::default(),
            pointer_visible: false,
            overlay: None,
            input_panel: None,
        }
    }
    pub fn set_show_stats(&mut self, enabled: bool) {
        self.overlay = enabled.then(crate::overlay::Overlay::new);
        self.dirty = true;
    }
    pub fn input_panel_open(&self) -> bool {
        self.input_panel.is_some()
    }
    pub(crate) fn open_input_panel(&mut self, panel: crate::input::panel::Panel) {
        self.input_panel = Some(panel);
        self.dirty = true;
    }
    pub(crate) fn close_input_panel(&mut self) {
        self.dirty |= self.input_panel.take().is_some();
    }
    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }
    pub fn mapping(&self, id: WindowId) -> Option<Mapping> {
        let window = self.windows.get(&id)?;
        let logical = size(window.geometry);
        let primary = self.primary.and_then(|id| self.windows.get(&id));
        if self.primary == Some(id) || window.fullscreen || primary.is_none() {
            return Some(Mapping::fit(logical));
        }
        let primary = primary.unwrap();
        let base = Mapping::fit(size(primary.geometry));
        let (left, top) = base.output((
            window.geometry.left.saturating_sub(primary.geometry.left),
            window.geometry.top.saturating_sub(primary.geometry.top),
        ));
        let mut viewport = base.viewport;
        viewport.left = left;
        viewport.top = top;
        Some(Mapping { logical, viewport })
    }
    pub fn pointer(&self) -> (i32, i32) {
        self.cursor
    }
    pub fn set_pointer(&mut self, position: (i32, i32)) {
        if self.cursor != position {
            self.dirty |= self.pointer_visible;
            if let Some(window) = self.focused.and_then(|id| self.windows.get_mut(&id))
                && window.cursor_state == 1
            {
                window.cursor_state = 0;
            }
        }
        self.cursor = position;
    }
    pub fn show_pointer(&mut self, visible: bool) {
        if self.pointer_visible != visible {
            self.pointer_visible = visible;
            self.dirty = true;
        }
    }
    pub fn post(&self, id: WindowId, input: Input) -> Result<(), String> {
        self.host.post(window::Event { window: id, input })
    }
    fn focus(&mut self, id: Option<WindowId>) -> Result<(), String> {
        if self.focused != id {
            if let Some(old) = self.focused.take() {
                self.post(old, Input::Focus(false))?;
            }
            self.focused = id;
            if let Some(id) = id {
                self.post(id, Input::Focus(true))?;
            }
        }
        Ok(())
    }
    fn visible(&self, id: WindowId) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|w| live(&w.alive) && w.visible && !w.minimized)
    }
    fn top(&self) -> Option<WindowId> {
        self.modals
            .last()
            .map(|m| m.request.window)
            .filter(|&id| self.visible(id))
            .or_else(|| {
                self.order
                    .iter()
                    .rev()
                    .copied()
                    .find(|&id| self.visible(id))
            })
    }
    fn raise(&mut self, id: WindowId) {
        self.order.retain(|&other| other != id);
        let index = if self.windows[&id].topmost {
            self.order.len()
        } else {
            self.order
                .iter()
                .position(|other| self.windows[other].topmost)
                .unwrap_or(self.order.len())
        };
        self.order.insert(index, id);
        self.dirty = true;
    }
    fn poll_lifetimes(&mut self) -> Result<(), String> {
        let mut restored = None;
        let mut completed = Vec::new();
        for index in (0..self.modals.len()).rev() {
            let modal = &self.modals[index];
            if live(&modal.active)
                && !modal.request.cancelled()
                && self
                    .windows
                    .get(&modal.request.window)
                    .is_some_and(|w| live(&w.alive))
            {
                continue;
            }
            let modal = self.modals.remove(index);
            restored = modal.restore;
            let mut geometry = Geometry::default();
            if let Some(window) = self.windows.get_mut(&modal.request.window) {
                window.visible = false;
                geometry = window.geometry;
            }
            completed.push((modal.request, geometry));
            self.dirty = true;
        }
        let before = self.windows.len();
        self.windows.retain(|_, window| live(&window.alive));
        self.order.retain(|id| self.windows.contains_key(id));
        if self
            .primary
            .is_some_and(|id| !self.windows.contains_key(&id))
        {
            self.primary = self.order.first().copied();
        }
        self.dirty |= before != self.windows.len();
        if !completed.is_empty() || self.focused.is_some_and(|id| !self.visible(id)) {
            let target = self
                .modals
                .last()
                .map(|m| m.request.window)
                .or(restored.filter(|&id| self.visible(id)))
                .or_else(|| self.top());
            self.focus(target)?;
        }
        // The waiting modal script resumes only after input ownership changed.
        for (request, geometry) in completed {
            request.complete(Ok(geometry));
        }
        Ok(())
    }
    pub fn pump(&mut self) -> Result<(), String> {
        self.poll_lifetimes()?;
        // Finish the published frame before executing later image mutations.
        // Otherwise its pinned versions force copy-on-write allocations even
        // though the old pixels could already have been composed and released.
        // Hosts may pump several times between presents, so keep this fence
        // across calls as well as within a single batch.
        if self.windows.values().any(|w| w.snapshot.is_some()) {
            return Ok(());
        }
        // Bound one UI turn so script drawing cannot starve the controller.
        for _ in 0..self.host.limits().operations {
            let Some(update) = self.host.next_update(self.completed) else {
                break;
            };
            match update {
                Update::Scenes(scenes) => {
                    let _stage = crate::watchdog::scope(crate::watchdog::Stage::CaptureScene);
                    for (id, mut scene) in scenes {
                        // Capture now, before consuming any later image command.
                        if self.windows.contains_key(&id) {
                            let mapping = self.mapping(id).unwrap();
                            let (logical, physical) = self.windows[&id].surface_sizes(
                                &scene,
                                mapping,
                                &self.graphics.gpu,
                            );
                            // Reclaim equivalent storage before capture pins all
                            // visible planes. A pinned snapshot prevents freeing
                            // the old large allocations during border compaction.
                            self.graphics.prepare_scene_capture(physical, &mut scene)?;
                            let snapshot = self
                                .graphics
                                .capture_prepared(scene, Some((logical, physical)))?;
                            // Capture may restore parked scene images. Reserve
                            // composition space again with those inputs pinned,
                            // so only off-scene intermediates can be evicted.
                            self.graphics.prepare_scene(physical)?;
                            self.windows.get_mut(&id).unwrap().snapshot = Some(snapshot);
                            self.dirty = true;
                        }
                    }
                    if self.windows.values().any(|w| w.snapshot.is_some()) {
                        break;
                    }
                }
                Update::Request(request) => {
                    self.completed = self.completed.max(request.sequence);
                    if request.cancelled() {
                        continue;
                    }
                    if let Command::ShowModal { active } = &request.command {
                        let result = self.begin_modal(request.window, active);
                        match result {
                            Ok(true) => self.modals.push(Modal {
                                active: active.clone(),
                                restore: self.focused,
                                request,
                            }),
                            Ok(false) => {
                                let geometry = self.windows[&request.window].geometry;
                                request.complete(Ok(geometry));
                            }
                            Err(error) => request.complete(Err(error)),
                        }
                        if let Some(id) = self.modals.last().map(|m| m.request.window) {
                            self.focus(Some(id))?;
                        }
                    } else {
                        let result = self.execute(request.window, &request.command);
                        if matches!(&result, Ok(Response::Done))
                            && let Command::Graphics(command) = &request.command
                            && let Some(batch) = self.graphics.prepare_draws(command)
                        {
                            request.offer_draws(batch);
                        }
                        request.respond(result);
                    }
                }
            }
        }
        self.poll_lifetimes()
    }
    fn begin_modal(&mut self, id: WindowId, active: &Weak<AtomicBool>) -> Result<bool, String> {
        let window = self.windows.get_mut(&id).ok_or("modal window is closed")?;
        if !live(active) {
            return Ok(false);
        }
        if window.visible || self.modals.iter().any(|m| m.request.window == id) {
            return Err("cannot show an already visible or modal window".into());
        }
        window.visible = true;
        window.minimized = false;
        self.raise(id);
        self.post(id, Input::Extended(window::ExtendedEvent::Show))?;
        Ok(true)
    }
    fn execute(&mut self, id: WindowId, command: &Command) -> Result<Response, String> {
        match command {
            Command::Graphics(command) => return self.graphics.execute(command),
            Command::Create { alive, .. } => {
                if self.windows.contains_key(&id) {
                    return Err("window already exists".into());
                }
                if self.windows.len() >= self.host.limits().windows {
                    return Err("Vita window capacity reached".into());
                }
                let geometry = geometry(
                    Size {
                        width: 640,
                        height: 480,
                    },
                    0,
                    0,
                );
                self.windows.insert(
                    id,
                    Window {
                        alive: alive.clone(),
                        geometry,
                        normal: geometry,
                        visible: false,
                        minimized: false,
                        maximized: false,
                        fullscreen: false,
                        topmost: false,
                        maximize_box: true,
                        minimize_box: true,
                        cursor_state: 0,
                        style: Default::default(),
                        snapshot: None,
                        canvas: None,
                        scene_state: SceneState::default(),
                        viewport: Default::default(),
                    },
                );
                self.order.push(id);
                self.primary.get_or_insert(id);
                return Ok(Response::Geometry(geometry));
            }
            Command::Icon(_) => return Ok(Response::Done),
            Command::Inform { caption, text, .. } => {
                // A game's exception handler often sends the original error
                // here. Preserve it even though native dialogs are unavailable.
                krkr_protocol::diagnostic!("[VITA][INFORM] {caption}: {text}");
                return Err(format!(
                    "Vita message dialog is unavailable: {caption}: {text}"
                ));
            }
            Command::SelectDirectory(_) => return Ok(Response::DirectorySelected(None)),
            Command::Desktop(command) => return self.desktop(command).map(Response::Desktop),
            Command::RegisterCursor { id, image } => {
                self.cursors
                    .register(*id, image, self.host.limits().cursors)?;
            }
            _ => {}
        }
        let window = self.windows.get_mut(&id).ok_or("window is closed")?;
        let old = window.geometry;
        let old_visible = window.visible;
        let mut activate = false;
        match command {
            Command::Position(left, top) => {
                window.geometry = geometry(size(old), *left, *top);
                if !window.fullscreen {
                    window.normal = window.geometry;
                }
            }
            Command::Size { width, height, .. } => {
                if *width == 0
                    || *height == 0
                    || *width > i32::MAX as u32
                    || *height > i32::MAX as u32
                {
                    return Err("invalid window dimensions".into());
                }
                if self.primary == Some(id) {
                    self.graphics.gpu.set_canvas_size(Size {
                        width: *width,
                        height: *height,
                    });
                }
                window.geometry = geometry(
                    Size {
                        width: *width,
                        height: *height,
                    },
                    old.left,
                    old.top,
                );
                if !window.fullscreen {
                    window.normal = window.geometry;
                }
            }
            Command::Visible(visible) => {
                window.visible = *visible;
                activate = *visible;
            }
            Command::Focus => activate = true,
            Command::StayOnTop(topmost) => window.topmost = *topmost,
            Command::FullScreen(full) => {
                if *full != window.fullscreen {
                    if *full {
                        window.normal = old;
                        window.geometry = geometry(DISPLAY, 0, 0);
                    } else {
                        window.geometry = window.normal;
                    }
                    window.fullscreen = *full;
                }
            }
            Command::Control(control) => {
                use window::Control::*;
                match control {
                    Minimize | Minimized(true) => window.minimized = true,
                    Maximize | Maximized(true) => {
                        window.maximized = true;
                        window.minimized = false;
                    }
                    Restore => {
                        window.maximized = false;
                        window.minimized = false;
                    }
                    Minimized(false) => window.minimized = false,
                    Maximized(false) => window.maximized = false,
                }
            }
            Command::MaximizeBox(value) => window.maximize_box = *value,
            Command::MinimizeBox(value) => window.minimize_box = *value,
            Command::HideCursor => window.cursor_state = 1,
            Command::CursorState(state) => window.cursor_state = *state,
            Command::InputStyle(style) => {
                if !self.cursors.contains(style.cursor) {
                    return Err("unknown cursor handle".into());
                }
                window.style = *style;
            }
            Command::Caption(_)
            | Command::MinSize(..)
            | Command::MaxSize(..)
            | Command::BorderStyle(_)
            | Command::BeginMove
            | Command::DisableResize(_)
            | Command::DisableMove(_)
            | Command::QueryState
            | Command::ZOrder { .. }
            | Command::CursorPosition(..)
            | Command::RegisterCursor { .. } => {}
            _ => return Err("invalid window request dispatch".into()),
        }
        let current = window.geometry;
        let visible = window.visible;
        if current != old {
            self.post(
                id,
                if size(current) != size(old) {
                    Input::Resize(current)
                } else {
                    Input::Move(current)
                },
            )?;
        }
        if old_visible != visible {
            self.post(
                id,
                Input::Extended(if visible {
                    window::ExtendedEvent::Show
                } else {
                    window::ExtendedEvent::Hide
                }),
            )?;
        }
        if let Command::ZOrder {
            order,
            activate: focus,
        } = command
        {
            match order {
                window::ZOrder::Top | window::ZOrder::Topmost => {
                    if matches!(order, window::ZOrder::Topmost) {
                        self.windows.get_mut(&id).unwrap().topmost = true;
                    }
                    self.raise(id);
                }
                window::ZOrder::NotTopmost => {
                    self.windows.get_mut(&id).unwrap().topmost = false;
                    self.raise(id);
                }
                window::ZOrder::Bottom => {
                    self.order.retain(|&other| other != id);
                    self.order.insert(0, id);
                }
                window::ZOrder::Behind(other) => {
                    if *other != id {
                        if !self.windows.contains_key(other) {
                            return Err("z-order target is closed".into());
                        }
                        self.order.retain(|&other| other != id);
                        let index = self
                            .order
                            .iter()
                            .position(|other_id| other_id == other)
                            .unwrap();
                        self.order.insert(index, id);
                    }
                }
            }
            activate |= *focus;
        }
        if matches!(command, Command::StayOnTop(_)) {
            self.raise(id);
        }
        if activate && self.visible(id) && self.modals.last().is_none_or(|m| m.request.window == id)
        {
            self.raise(id);
            self.focus(Some(id))?;
        }
        if let Command::CursorPosition(x, y) = command {
            self.set_pointer(self.mapping(id).unwrap().output((*x, *y)));
        }
        self.dirty |= matches!(
            command,
            Command::HideCursor
                | Command::CursorState(_)
                | Command::InputStyle(_)
                | Command::RegisterCursor { .. }
        ) && self.pointer_visible;
        self.dirty |= matches!(
            command,
            Command::Visible(_)
                | Command::ZOrder { .. }
                | Command::StayOnTop(_)
                | Command::Control(_)
                | Command::FullScreen(_)
        ) || current != old;
        if matches!(
            command,
            Command::QueryState
                | Command::Control(_)
                | Command::MaximizeBox(_)
                | Command::MinimizeBox(_)
                | Command::DisableResize(_)
                | Command::ZOrder { .. }
        ) {
            let window = &self.windows[&id];
            Ok(Response::Snapshot(window::Snapshot {
                geometry: current,
                visible,
                outer: Some(rectangle(current)),
                client: Some(rectangle(current)),
                normal: Some(rectangle(window.normal)),
                normal_workspace: Some(rectangle(window.normal)),
                maximized: window.maximized,
                minimized: Some(window.minimized),
                maximize_box: Some(window.maximize_box),
                minimize_box: Some(window.minimize_box),
            }))
        } else {
            Ok(Response::Geometry(current))
        }
    }
    fn desktop(
        &mut self,
        command: &window::desktop::Command,
    ) -> Result<window::desktop::Response, String> {
        use window::desktop::{Command as C, Response as R};
        let bounds = rectangle(geometry(DISPLAY, 0, 0));
        let monitor = || window::desktop::Monitor {
            name: "PS Vita".into(),
            primary: true,
            monitor: bounds,
            work: Some(bounds),
        };
        Ok(match command {
            C::Monitors(_) => R::Monitors(Some(vec![(monitor(), bounds)])),
            C::Monitor { .. } => R::Monitor(Some(monitor())),
            C::Cursor => R::Point(Some(self.cursor)),
            C::SetCursor(x, y) => {
                self.cursor = (*x, *y);
                R::Void
            }
            C::Clip(window::desktop::Clip::Release) => R::Void,
            C::Clip(_) => return Err("Vita cursor clipping is unavailable".into()),
            C::Metric(metric) => R::Integer(match metric {
                0 | 16 | 59 | 61 | 78 => i64::from(DISPLAY.width),
                1 | 17 | 60 | 62 | 79 => i64::from(DISPLAY.height),
                80 => 1,
                _ => 0,
            }),
            C::DoubleClickTime => R::Integer(500),
            C::MapKey { .. } => return Err("Vita has no native keyboard scan-code table".into()),
            C::IconicPreview(_) | C::Corner { .. } | C::Ime { .. } => R::Bool(false),
        })
    }
    /// Rasterize dirty scenes once. Redraws keep only a completed canvas, so
    /// game edits no longer detach obsolete versions from the last frame.
    pub fn render(&mut self) -> Result<bool, String> {
        let _stage = crate::watchdog::scope(crate::watchdog::Stage::Render);
        let timer = krkr_protocol::diagnostics::Timer::start();
        let result = self.render_inner();
        timer.report(|| "stage=render".into());
        result
    }
    fn render_inner(&mut self) -> Result<bool, String> {
        if let Some(panel) = &mut self.input_panel {
            match panel.refresh(&self.graphics.gpu) {
                Ok(changed) => self.dirty |= changed,
                Err(error) => {
                    // An optional input UI must not turn temporary texture
                    // pressure into a game failure. Input releases on next poll.
                    krkr_protocol::log!(Warn, "[VITA][INPUT] panel unavailable: {error}");
                    self.close_input_panel();
                }
            }
        }
        if let Some(overlay) = &mut self.overlay {
            self.dirty |= overlay.refresh(&self.graphics.gpu, std::time::Instant::now())?;
        }
        if !self.dirty {
            return Ok(false);
        }
        let new_frame =
            self.overlay.is_some() && self.windows.values().any(|w| w.snapshot.is_some());
        for id in self.order.clone() {
            let mapping = self.mapping(id).unwrap();
            let window = self.windows.get_mut(&id).unwrap();
            let Some(mut snapshot) = window.snapshot.take() else {
                continue;
            };
            let (logical, physical) =
                window.surface_sizes(&snapshot.scene, mapping, &self.graphics.gpu);
            let scene_stage = crate::watchdog::scope(crate::watchdog::Stage::SceneRender);
            self.graphics
                .gpu
                .update_scene_surface(
                    &mut window.canvas,
                    &mut window.scene_state,
                    logical,
                    physical,
                    &snapshot.scene,
                    &snapshot.images,
                )
                .map_err(|e| self.graphics.report_failure("scene render", e))?;
            window.viewport = snapshot.scene.viewport;
            drop(scene_stage);
            let _release = crate::watchdog::scope(crate::watchdog::Stage::ReleaseSnapshot);
            snapshot.release_images();
        }
        // Keep the launcher's loading screen on the front buffer until a
        // visible game canvas is ready. Hidden snapshots must still be consumed
        // above so startup commands can pass their frame fences.
        if !self.presented_scene
            && !self
                .order
                .iter()
                .any(|&id| self.visible(id) && self.windows[&id].canvas.is_some())
        {
            self.dirty = false;
            return Ok(false);
        }
        self.graphics
            .gpu
            .clear_display(DISPLAY)
            .map_err(|e| e.to_string())?;
        for &id in &self.order {
            if !self.visible(id) {
                continue;
            }
            let window = &self.windows[&id];
            let Some(canvas) = &window.canvas else {
                continue;
            };
            let mapping = self.mapping(id).unwrap();
            let dest = mapping.map_rect(window.viewport.destination(canvas.size));
            self.graphics
                .gpu
                .present_window(canvas, DISPLAY, dest, mapping.destination())
                .map_err(|e| e.to_string())?;
        }
        if self.pointer_visible
            && let Some(window) = self.focused.and_then(|id| self.windows.get(&id))
            && window.cursor_state == 0
            && window.style.cursor != -1
        {
            self.cursors
                .draw(&self.graphics.gpu, window.style.cursor, self.cursor)?;
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.draw(&self.graphics.gpu)?;
            if new_frame {
                overlay.frame();
            }
        }
        if let Some(panel) = &self.input_panel {
            panel.draw(&self.graphics.gpu)?;
        }
        self.graphics.gpu.resolve().map_err(|e| e.to_string())?;
        self.presented_scene = true;
        self.dirty = false;
        Ok(true)
    }
}
