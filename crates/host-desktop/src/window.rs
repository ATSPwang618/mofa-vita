//! The OS event loop stays on the calling (main) thread. VM construction and
//! execution happen inside the worker closure; a Heap never needs Send.
mod desktop;
mod dialogs;
mod extension;
mod graphics;
mod icons;
mod input;
mod modal;
mod overlay;
pub use overlay::set_enabled as set_show_stats;
mod presentation;
mod styles;
use krkr_protocol::window::{
    self as protocol, Client, Command, Geometry, Host, Input, Request, WindowId,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Fullscreen, Window, WindowId as NativeId, WindowLevel},
};

#[derive(Clone, Copy)]
enum Wake {
    Commands,
    Finished,
}

struct Native {
    id: WindowId,
    window: Arc<Window>,
    surface: Option<graphics::Surface>,
    visible: bool,
    scene: graphics::SceneSnapshot,
    alive: Weak<AtomicBool>,
    resize: Option<Request>,
    control: Option<extension::PendingControl>,
    normal: Option<protocol::Rectangle>,
    border_style: protocol::BorderStyle,
    disable_resize: bool,
    disable_move: bool,
    extension_state: Option<(Option<bool>, bool, bool)>,
    restore_maximized: bool,
    input: input::State,
    cursor: PhysicalPosition<f64>,
    hidden_at: Option<PhysicalPosition<f64>>,
    cursor_state: i32,
    input_style: krkr_protocol::input_style::Style,
    icon: Option<Arc<icons::NativeIcon>>,
}
impl Native {
    fn geometry(&self) -> Geometry {
        let inner = self.window.inner_size();
        let outer = self.window.outer_size();
        let position = self.window.outer_position().unwrap_or_default();
        let client = self.window.inner_position().unwrap_or(position);
        Geometry {
            left: position.x,
            top: position.y,
            client_left: client.x,
            client_top: client.y,
            width: outer.width,
            height: outer.height,
            inner_width: inner.width,
            inner_height: inner.height,
        }
    }
    fn alive(&self) -> bool {
        self.alive
            .upgrade()
            .is_some_and(|alive| alive.load(Ordering::Acquire))
    }
    fn inner_size(&self, width: u32, height: u32) -> PhysicalSize<u32> {
        let geometry = self.geometry();
        PhysicalSize::new(
            width
                .saturating_sub(geometry.width.saturating_sub(geometry.inner_width))
                .max(1),
            height
                .saturating_sub(geometry.height.saturating_sub(geometry.inner_height))
                .max(1),
        )
    }
}
struct App<F, T> {
    start: Option<(F, Client)>,
    worker: Option<JoinHandle<Result<T, String>>>,
    host: Host,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
    notified: Arc<AtomicBool>,
    windows: HashMap<NativeId, Native>,
    ids: HashMap<WindowId, NativeId>,
    main_window: Option<NativeId>,
    modals: Vec<modal::Session>,
    application_icon: Option<Arc<icons::NativeIcon>>,
    directory_dialogs: dialogs::Dialogs,
    resumed: bool,
    graphics: Option<graphics::Graphics>,
    completed_sequence: u64,
    display_monitors: Vec<protocol::desktop::Monitor>,
    next_display_refresh: Option<Instant>,
    show_stats: bool,
    cursors: HashMap<
        i32,
        (
            Weak<krkr_protocol::input_style::CursorImage>,
            winit::window::CustomCursor,
        ),
    >,
}
impl<F, T> App<F, T> {
    fn refresh_display(&mut self, event_loop: &ActiveEventLoop) {
        // A UI constructor can yield thousands of drawing requests. Display
        // topology is not frame data; do not enumerate the OS on every wake.
        let now = Instant::now();
        if self.next_display_refresh.is_some_and(|next| now < next) {
            return;
        }
        self.next_display_refresh = Some(now + Duration::from_secs(1));
        let monitors = desktop::monitors(event_loop);
        if monitors != self.display_monitors {
            if !self.display_monitors.is_empty() {
                for native in self.windows.values() {
                    let _ = self.host.post(protocol::Event {
                        window: native.id,
                        input: Input::Extended(protocol::ExtendedEvent::DisplayChanged),
                    });
                }
            }
            self.display_monitors = monitors;
        }
        let monitor = self
            .main_window
            .and_then(|id| self.windows.get(&id))
            .and_then(|native| native.window.current_monitor())
            .or_else(|| event_loop.primary_monitor())
            .or_else(|| event_loop.available_monitors().next());
        self.host.set_display(monitor.map(|monitor| {
            let size = monitor.size();
            let position = monitor.position();
            // winit has no portable work-area query. Keep this adaptation
            // explicit: the desktop rectangle includes system panels.
            protocol::Display {
                width: size.width,
                height: size.height,
                desktop_left: position.x,
                desktop_top: position.y,
                desktop_width: size.width,
                desktop_height: size.height,
            }
        }));
    }
    fn commands(&mut self, event_loop: &ActiveEventLoop) {
        self.notified.store(false, Ordering::Release);
        self.poll_directory_dialogs();
        self.cursors
            .retain(|_, (image, _)| image.strong_count() != 0);
        if !self.resumed {
            return;
        }
        self.poll_modals();
        self.windows.retain(|_, native| {
            if let Some(request) = &native.resize
                && request.cancelled()
            {
                native.resize = None;
            }
            if native.alive() {
                true
            } else {
                self.ids.remove(&native.id);
                false
            }
        });
        // The protocol's operation admission limit also bounds this drain.
        for index in 0..self.host.limits().operations {
            let Some(update) = self.host.next_update(self.completed_sequence) else {
                break;
            };
            if index + 1 == self.host.limits().operations
                && !self.notified.swap(true, Ordering::AcqRel)
            {
                // A scene fence also consumes a slot. Continue a full batch
                // even when no new producer wake arrives after this drain.
                let _ = self.proxy.send_event(Wake::Commands);
            }
            let request = match update {
                protocol::Update::Request(request) => request,
                protocol::Update::Scenes(scenes) => {
                    if let Some(graphics) = &mut self.graphics {
                        graphics.commit();
                    }
                    for (id, scene) in scenes {
                        if let Some(native) =
                            self.ids.get(&id).and_then(|id| self.windows.get_mut(id))
                        {
                            match graphics::SceneSnapshot::capture(scene, self.graphics.as_ref()) {
                                Ok(scene) => native.scene = scene,
                                Err(error) => {
                                    self.host.disconnect(error);
                                    event_loop.exit();
                                    return;
                                }
                            }
                            if let Some(surface) = &mut native.surface {
                                surface.invalidate_scene();
                            }
                            native.window.request_redraw();
                        }
                    }
                    continue;
                }
            };
            self.completed_sequence = request.sequence;
            if request.cancelled() {
                continue;
            }
            if matches!(request.command, Command::ShowModal { .. }) {
                self.show_modal(request);
                continue;
            }
            if let Command::Create { alive, caption } = &request.command {
                if !alive.upgrade().is_some_and(|a| a.load(Ordering::Acquire)) {
                    continue;
                }
                match event_loop.create_window(
                    Window::default_attributes()
                        .with_visible(false)
                        .with_active(false)
                        .with_title(caption)
                        .with_inner_size(PhysicalSize::new(640, 480)),
                ) {
                    Ok(window) => {
                        if self.windows.is_empty() {
                            self.main_window = Some(window.id());
                        }
                        let mut native = Native {
                            id: request.window,
                            window: Arc::new(window),
                            surface: None,
                            visible: false,
                            scene: Default::default(),
                            alive: alive.clone(),
                            resize: None,
                            control: None,
                            normal: None,
                            border_style: Default::default(),
                            disable_resize: false,
                            disable_move: false,
                            extension_state: None,
                            restore_maximized: false,
                            input: input::State::default(),
                            cursor: PhysicalPosition::new(0.0, 0.0),
                            hidden_at: None,
                            cursor_state: 0,
                            input_style: Default::default(),
                            icon: None,
                        };
                        native.initialize_icons(self.application_icon.as_ref());
                        let geometry = native.snapshot().geometry;
                        native.observe_extensions(&self.host);
                        native.window.set_ime_allowed(false);
                        self.ids.insert(request.window, native.window.id());
                        self.windows.insert(native.window.id(), native);
                        self.refresh_display(event_loop);
                        request.complete(Ok(geometry));
                    }
                    Err(error) => request.complete(Err(error.to_string())),
                }
                continue;
            }
            if matches!(
                request.command,
                Command::SelectDirectory(_) | Command::Inform { .. }
            ) {
                self.queue_directory_dialog(request);
                continue;
            }
            if let Command::Icon(command) = &request.command {
                let result = self.apply_icons(event_loop, request.window, command);
                request.respond(result.map(|()| protocol::Response::Done));
                continue;
            }
            if let Command::Desktop(command) = &request.command {
                let result = self.desktop(event_loop, command);
                request.respond(result.map(protocol::Response::Desktop));
                continue;
            }
            let Some(native) = self
                .ids
                .get(&request.window)
                .and_then(|id| self.windows.get_mut(id))
            else {
                request.complete(Err("window is closed".into()));
                continue;
            };
            match &request.command {
                Command::RegisterCursor { id, image } => {
                    let source = winit::window::CustomCursor::from_rgba(
                        image.rgba.as_slice().to_vec(),
                        image.width,
                        image.height,
                        image.hotspot.0,
                        image.hotspot.1,
                    );
                    match source {
                        Ok(source) => {
                            self.cursors.insert(
                                *id,
                                (
                                    Arc::downgrade(image),
                                    event_loop.create_custom_cursor(source),
                                ),
                            );
                        }
                        Err(error) => {
                            request.complete(Err(error.to_string()));
                            continue;
                        }
                    }
                }
                Command::Graphics(_) => {
                    let ready = if let Some(graphics) = &self.graphics {
                        if native.surface.is_none() {
                            graphics
                                .surface(native.window.clone())
                                .map(|surface| native.surface = Some(surface))
                        } else {
                            Ok(())
                        }
                    } else {
                        graphics::Graphics::new(
                            native.window.clone(),
                            self.host.staging_budget(),
                            self.host.image_cache(),
                        )
                        .map(|(graphics, surface)| {
                            self.host
                                .set_transition_kernels(graphics.gpu.transition_kernels());
                            native.surface = Some(surface);
                            self.graphics = Some(graphics);
                        })
                    };
                    if let Err(error) = ready {
                        request.respond(Err(error));
                        continue;
                    }
                    self.graphics
                        .as_mut()
                        .expect("graphics initialized")
                        .request(request);
                    // Resource operations can be steps of one script update.
                    // Present when the engine publishes its completed scene.
                    continue;
                }
                Command::Create { .. } => unreachable!(),
                Command::ShowModal { .. } => unreachable!(),
                Command::Icon(_) | Command::SelectDirectory(_) | Command::Inform { .. } => {
                    unreachable!()
                }
                Command::Desktop(_) => unreachable!(),
                Command::DisableMove(disabled) => {
                    if *disabled && native.window.is_decorated() {
                        request.complete(Err(
                            "this backend cannot disable movement from a native title bar".into(),
                        ));
                        continue;
                    }
                    native.disable_move = *disabled;
                }
                Command::QueryState => {
                    request.respond(Ok(protocol::Response::Snapshot(native.snapshot())));
                    continue;
                }
                Command::Control(action) => {
                    let action = *action;
                    native.start_control(request, action);
                    continue;
                }
                Command::MaximizeBox(enabled) | Command::MinimizeBox(enabled) => {
                    let maximize = matches!(request.command, Command::MaximizeBox(_));
                    let result = native
                        .change_button(maximize, *enabled)
                        .map(|()| protocol::Response::Snapshot(native.snapshot()));
                    request.respond(result);
                    continue;
                }
                Command::DisableResize(disabled) => {
                    let result = native
                        .disable_resize(*disabled)
                        .map(|()| protocol::Response::Snapshot(native.snapshot()));
                    request.respond(result);
                    continue;
                }
                Command::ZOrder { order, activate } => {
                    #[cfg(target_os = "linux")]
                    let levels = {
                        use winit::platform::wayland::ActiveEventLoopExtWayland;
                        !event_loop.is_wayland()
                    };
                    #[cfg(not(target_os = "linux"))]
                    let levels = true;
                    let result = native
                        .z_order(*order, *activate, levels)
                        .map(|()| protocol::Response::Snapshot(native.snapshot()));
                    request.respond(result);
                    continue;
                }
                Command::Caption(text) => native.window.set_title(text),
                Command::Visible(visible) => {
                    native.visible = *visible;
                    native.window.set_visible(*visible);
                    if *visible {
                        native.window.request_redraw();
                    } else if let Some(surface) = &mut native.surface {
                        // Retain the completed canvas for showing the window
                        // again, and release its client-sized presentation copy.
                        surface.release_frame();
                    }
                }
                Command::Position(x, y) => native
                    .window
                    .set_outer_position(PhysicalPosition::new(*x, *y)),
                Command::Size {
                    width,
                    height,
                    inner,
                } => {
                    let size = if *inner {
                        PhysicalSize::new(*width, *height)
                    } else {
                        native.inner_size(*width, *height)
                    };
                    if size != native.window.inner_size()
                        && native.window.request_inner_size(size).is_none()
                    {
                        native.resize = Some(request);
                        continue;
                    }
                }
                Command::MinSize(w, h) => native
                    .window
                    .set_min_inner_size((*w != 0 || *h != 0).then(|| native.inner_size(*w, *h))),
                Command::MaxSize(w, h) => {
                    let enabled = *w != 0 || *h != 0;
                    let (w, h) = (
                        if *w == 0 { u32::MAX / 2 } else { *w },
                        if *h == 0 { u32::MAX / 2 } else { *h },
                    );
                    native
                        .window
                        .set_max_inner_size(enabled.then(|| native.inner_size(w, h)));
                }
                Command::Focus => native.window.focus_window(),
                Command::BorderStyle(style) => {
                    if native.disable_move && *style != protocol::BorderStyle::None {
                        request.complete(Err(
                            "a move-disabled window cannot add a native title bar on this backend"
                                .into(),
                        ));
                        continue;
                    }
                    native.set_border_style(*style);
                }
                Command::BeginMove => {
                    if native.disable_move {
                        request.complete(Ok(native.geometry()));
                        continue;
                    }
                    if let Err(error) = native.window.drag_window() {
                        request.complete(Err(error.to_string()));
                        continue;
                    }
                }
                Command::StayOnTop(value) => native.window.set_window_level(if *value {
                    WindowLevel::AlwaysOnTop
                } else {
                    WindowLevel::Normal
                }),
                Command::FullScreen(value) => {
                    native.snapshot();
                    native
                        .window
                        .set_fullscreen(value.then_some(Fullscreen::Borderless(None)));
                }
                Command::HideCursor => {
                    native.set_cursor_state(1);
                }
                Command::CursorState(state) => native.set_cursor_state(*state),
                Command::CursorPosition(x, y) => {
                    if let Err(error) = native
                        .window
                        .set_cursor_position(PhysicalPosition::new(*x, *y))
                    {
                        request.complete(Err(error.to_string()));
                        continue;
                    }
                    native.cursor = PhysicalPosition::new(f64::from(*x), f64::from(*y));
                    native.input.set_position(*x, *y);
                    if native.cursor_state == 1 {
                        native.set_cursor_state(0);
                    }
                }
                Command::InputStyle(style) => {
                    if let Err(error) = native.set_input_style(*style, &self.cursors) {
                        request.complete(Err(error));
                        continue;
                    }
                }
            }
            let geometry = native.geometry();
            native.observe_extensions(&self.host);
            self.refresh_display(event_loop);
            request.complete(Ok(geometry));
        }
    }
}
impl<F, T> ApplicationHandler<Wake> for App<F, T>
where
    F: FnOnce(Client) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.resumed = true;
        self.next_display_refresh = None;
        self.refresh_display(event_loop);
        if let Some((start, client)) = self.start.take() {
            let proxy = self.proxy.clone();
            self.worker = Some(std::thread::spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| start(client)))
                        .unwrap_or_else(|_| Err("engine worker panicked".into()));
                let _ = proxy.send_event(Wake::Finished);
                result
            }));
        }
        self.commands(event_loop);
    }
    fn suspended(&mut self, _: &ActiveEventLoop) {
        self.resumed = false;
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Commands => self.commands(event_loop),
            Wake::Finished => event_loop.exit(),
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: NativeId, event: WindowEvent) {
        let Some(native) = self.windows.get_mut(&id) else {
            return;
        };
        if !native.alive() {
            self.commands(event_loop);
            return;
        }
        if let Some(modal) = self.modals.last()
            && modal.window != id
        {
            if matches!(
                event,
                WindowEvent::Focused(true)
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::CloseRequested
            ) && let Some(dialog) = self.windows.get(&modal.window)
            {
                dialog.window.focus_window();
            }
            if modal::user_input(&event) {
                return;
            }
        }
        let native = self.windows.get_mut(&id).expect("existing window");
        match event {
            WindowEvent::Resized(_) => {
                if let Some(pending) = &mut native.control {
                    pending.resized = true;
                }
                native.snapshot();
                native.observe_extensions(&self.host);
                let geometry = native.geometry();
                if let Some(request) = native.resize.take() {
                    request.complete(Ok(geometry));
                }
                if self
                    .host
                    .post(protocol::Event {
                        window: native.id,
                        input: Input::Resize(geometry),
                    })
                    .is_err()
                {
                    event_loop.exit();
                }
            }
            WindowEvent::Moved(_) => {
                native.snapshot();
                if self
                    .host
                    .post(protocol::Event {
                        window: native.id,
                        input: Input::Move(native.geometry()),
                    })
                    .is_err()
                {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                if native.visible
                    && let (Some(graphics), Some(surface)) =
                        (&mut self.graphics, &mut native.surface)
                    && let Err(error) = surface.draw(graphics, &native.window, &mut native.scene)
                {
                    self.host.disconnect(error);
                    event_loop.exit();
                }
            }
            other => {
                match &other {
                    WindowEvent::Focused(active) => {
                        let _ = self.host.post(protocol::Event {
                            window: native.id,
                            input: Input::Extended(protocol::ExtendedEvent::ActivateChanged(
                                *active,
                                native.window.is_minimized(),
                            )),
                        });
                    }
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let dpi = (*scale_factor * 96.0).round().clamp(1.0, u32::MAX as f64) as u32;
                        let _ = self.host.post(protocol::Event {
                            window: native.id,
                            input: Input::Extended(protocol::ExtendedEvent::DpiChanged(dpi, dpi)),
                        });
                    }
                    _ => {}
                }
                if let WindowEvent::CursorMoved { position, .. } = &other {
                    native.cursor = *position;
                    if native.hidden_at.is_some_and(|hidden| hidden != *position) {
                        native.set_cursor_state(0);
                    }
                }
                if let Err(error) = native.input.deliver(other, native.id, &self.host) {
                    self.host.disconnect(error);
                    event_loop.exit();
                }
            }
        }
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.host.disconnect("desktop event loop stopped".into());
        self.windows.clear();
        self.ids.clear();
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.refresh_display(event_loop);
        if let Some(graphics) = &mut self.graphics {
            match graphics.poll() {
                Ok(true) => event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(1),
                )),
                Ok(false) => event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait),
                Err(error) => {
                    self.host.disconnect(error);
                    event_loop.exit();
                }
            }
        }
        let mut pending_control = false;
        for native in self.windows.values_mut() {
            pending_control |= native.poll_control();
            native.observe_extensions(&self.host);
        }
        if pending_control {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(10),
            ));
        }
        // Keep an idle game's HUD current without driving its script timers or
        // running a continuous redraw loop. Other pending deadlines win.
        if self.show_stats {
            let now = Instant::now();
            let mut next_overlay: Option<Instant> = None;
            for native in self.windows.values_mut().filter(|n| {
                n.visible
                    && n.window.is_minimized() != Some(true)
                    && n.window.inner_size().width > 0
                    && n.window.inner_size().height > 0
            }) {
                if let Some(surface) = &mut native.surface
                    && let Some(next) = surface.overlay_deadline()
                {
                    if now >= next {
                        native.window.request_redraw();
                    } else {
                        next_overlay = Some(next_overlay.map_or(next, |old| old.min(next)));
                    }
                }
            }
            if let Some(next) = next_overlay {
                use winit::event_loop::ControlFlow;
                match event_loop.control_flow() {
                    ControlFlow::Wait => event_loop.set_control_flow(ControlFlow::WaitUntil(next)),
                    ControlFlow::WaitUntil(old) if next < old => {
                        event_loop.set_control_flow(ControlFlow::WaitUntil(next))
                    }
                    _ => {}
                }
            }
        }
    }
}

pub fn run<F, T>(start: F) -> Result<T, String>
where
    F: FnOnce(Client) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let wake_proxy = proxy.clone();
    let notified = Arc::new(AtomicBool::new(false));
    let wake_notified = notified.clone();
    let (client, host) = protocol::channel(
        protocol::Limits {
            staging_bytes: crate::memory::STAGING_BYTES,
            image_cache_bytes: crate::memory::CACHE_BYTES,
            ..Default::default()
        },
        Arc::new(move || {
            // EventLoopProxy itself is unbounded; use one outstanding wake token.
            if !wake_notified.swap(true, Ordering::AcqRel) {
                let _ = wake_proxy.send_event(Wake::Commands);
            }
        }),
    );
    let mut app = App {
        start: Some((start, client)),
        worker: None,
        host,
        proxy,
        notified,
        windows: HashMap::new(),
        ids: HashMap::new(),
        main_window: None,
        modals: Vec::new(),
        application_icon: None,
        directory_dialogs: Default::default(),
        resumed: false,
        graphics: None,
        completed_sequence: 0,
        display_monitors: Vec::new(),
        next_display_refresh: None,
        show_stats: overlay::enabled(),
        cursors: HashMap::new(),
    };
    let result = event_loop.run_app(&mut app).map_err(|e| e.to_string());
    app.host.disconnect("desktop event loop stopped".into());
    // No mailbox or world borrow is held while waiting for the worker.
    let worker = app.worker.take().map(|worker| {
        worker
            .join()
            .unwrap_or_else(|_| Err("engine worker panicked".into()))
    });
    result?;
    worker.unwrap_or_else(|| Err("desktop event loop did not start".into()))
}
