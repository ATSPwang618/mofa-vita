//! Desktop presentation of the engine's logical modal scopes. No OS dialog
//! templates, native handles, or nested platform event loop are involved.
use super::*;

pub(super) struct Session {
    pub window: NativeId,
    restore: Option<NativeId>,
    active: Weak<AtomicBool>,
    request: Request,
}

impl<F, T> App<F, T> {
    pub(super) fn show_modal(&mut self, request: Request) {
        self.poll_modals();
        let Command::ShowModal { active } = &request.command else {
            unreachable!()
        };
        let Some(&id) = self.ids.get(&request.window) else {
            request.complete(Err("modal window is closed".into()));
            return;
        };
        let native = &self.windows[&id];
        if !active
            .upgrade()
            .is_some_and(|active| active.load(Ordering::Acquire))
        {
            request.complete(Ok(native.geometry()));
            return;
        }
        if native.visible || self.modals.iter().any(|modal| modal.window == id) {
            request.complete(Err("cannot show an already visible or modal window".into()));
            return;
        }
        let restore = self
            .modals
            .last()
            .map(|modal| modal.window)
            .or_else(|| {
                self.windows
                    .iter()
                    .find(|(_, native)| native.window.has_focus())
                    .map(|(&id, _)| id)
            })
            .or(self.main_window);
        self.reset_modal_input();
        let native = self.windows.get_mut(&id).expect("existing modal window");
        native.visible = true;
        native.window.set_visible(true);
        native.window.focus_window();
        native.window.request_redraw();
        native.observe_extensions(&self.host);
        self.modals.push(Session {
            window: id,
            restore,
            active: active.clone(),
            request,
        });
    }

    pub(super) fn poll_modals(&mut self) {
        let mut changed = false;
        let mut restore = None;
        let mut completed = Vec::new();
        // Top-down cleanup also covers cancelling nested script contexts.
        for index in (0..self.modals.len()).rev() {
            let modal = &self.modals[index];
            let live = self.windows.get(&modal.window).is_some_and(Native::alive);
            let active = modal
                .active
                .upgrade()
                .is_some_and(|active| active.load(Ordering::Acquire));
            if live && active && !modal.request.cancelled() {
                continue;
            }
            let modal = self.modals.remove(index);
            changed = true;
            restore = modal.restore;
            let geometry = if let Some(native) = self.windows.get_mut(&modal.window) {
                native.visible = false;
                native.window.set_visible(false);
                if let Some(surface) = &mut native.surface {
                    surface.release_frame();
                }
                native.observe_extensions(&self.host);
                native.geometry()
            } else {
                Geometry::default()
            };
            completed.push((modal.request, geometry));
        }
        if changed {
            self.reset_modal_input();
            let target = [
                self.modals.last().map(|modal| modal.window),
                restore,
                self.main_window,
            ]
            .into_iter()
            .flatten()
            .filter_map(|id| self.windows.get(&id))
            .find(|native| native.alive() && native.visible);
            if let Some(native) = target {
                native.window.focus_window();
            }
        }
        for (request, geometry) in completed {
            request.complete(Ok(geometry));
        }
    }

    fn reset_modal_input(&mut self) {
        for native in self.windows.values_mut() {
            native.input.cancel_gesture();
            let _ = native
                .window
                .set_cursor_grab(winit::window::CursorGrabMode::None);
        }
    }
}

pub(super) fn user_input(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::CloseRequested
            | WindowEvent::CursorEntered { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::ModifiersChanged(_)
            | WindowEvent::Ime(_)
            | WindowEvent::Touch(_)
            | WindowEvent::DroppedFile(_)
            | WindowEvent::HoveredFile(_)
            | WindowEvent::HoveredFileCancelled
    )
}
