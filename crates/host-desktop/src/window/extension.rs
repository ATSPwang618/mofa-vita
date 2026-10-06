//! Window-manager controls. Completion is bounded and follows observed state.
use super::{Native, Request};
use krkr_protocol::window::{Control, Rectangle, Response, Snapshot, ZOrder};
use std::time::{Duration, Instant};
use winit::window::{WindowButtons, WindowLevel};

pub(super) struct PendingControl {
    request: Request,
    minimized: Option<bool>,
    maximized: bool,
    needs_resize: bool,
    pub resized: bool,
    earliest: Instant,
    deadline: Instant,
}

impl Native {
    pub(super) fn observe_extensions(&mut self, host: &krkr_protocol::window::Host) {
        use krkr_protocol::window::{Event, ExtendedEvent, Input};
        let minimized = self.window.is_minimized();
        let maximized = minimized != Some(true) && self.window.is_maximized();
        let visible = self.window.is_visible().unwrap_or(self.visible);
        if let Some((old_min, old_max, old_visible)) = self
            .extension_state
            .replace((minimized, maximized, visible))
        {
            let post = |kind| {
                let _ = host.post(Event {
                    window: self.id,
                    input: Input::Extended(kind),
                });
            };
            if minimized == Some(true) && old_min != Some(true) {
                post(ExtendedEvent::Minimize);
            }
            if maximized && !old_max {
                post(ExtendedEvent::Maximize);
            }
            if visible != old_visible {
                post(if visible {
                    ExtendedEvent::Show
                } else {
                    ExtendedEvent::Hide
                });
            }
        }
    }
    pub(super) fn snapshot(&mut self) -> Snapshot {
        let geometry = self.geometry();
        let minimized = self.window.is_minimized();
        // Win32 IsZoomed is false while iconic. Some winit backends retain the
        // prior maximize flag; keep that restore state separate from this query.
        let maximized = minimized != Some(true) && self.window.is_maximized();
        let outer = self.window.outer_position().ok().map(|p| Rectangle {
            x: p.x,
            y: p.y,
            width: geometry.width,
            height: geometry.height,
        });
        let client = self.window.inner_position().ok().map(|p| Rectangle {
            x: p.x,
            y: p.y,
            width: geometry.inner_width,
            height: geometry.inner_height,
        });
        if self.control.is_none() && minimized == Some(false) {
            self.restore_maximized = maximized;
            if !maximized && self.window.fullscreen().is_none() {
                self.normal = outer;
            }
        }
        let buttons = self.window.enabled_buttons();
        // winit's Linux getters return `all` even when the backend cannot set
        // buttons. Do not expose that fallback as an observed OS capability.
        let has_buttons = cfg!(any(target_os = "windows", target_os = "macos"));
        Snapshot {
            geometry,
            visible: self.window.is_visible().unwrap_or(self.visible),
            outer,
            client,
            normal: self.normal,
            normal_workspace: None,
            maximized,
            minimized,
            maximize_box: has_buttons.then_some(buttons.contains(WindowButtons::MAXIMIZE)),
            minimize_box: has_buttons.then_some(buttons.contains(WindowButtons::MINIMIZE)),
        }
    }

    pub(super) fn change_button(&mut self, maximize: bool, enabled: bool) -> Result<(), String> {
        if !cfg!(any(target_os = "windows", target_os = "macos")) {
            return Err("window buttons are unavailable on this backend".into());
        }
        let button = if maximize {
            WindowButtons::MAXIMIZE
        } else {
            WindowButtons::MINIMIZE
        };
        let mut buttons = self.window.enabled_buttons();
        buttons.set(button, enabled);
        self.window.set_enabled_buttons(buttons);
        if self.window.enabled_buttons().contains(button) != enabled {
            return Err("window manager did not apply the window button change".into());
        }
        Ok(())
    }

    pub(super) fn disable_resize(&mut self, disabled: bool) -> Result<(), String> {
        let resize = !disabled
            && matches!(
                self.border_style,
                krkr_protocol::window::BorderStyle::Sizeable
                    | krkr_protocol::window::BorderStyle::SizeToolWin
            );
        let buttons = self.window.enabled_buttons();
        self.window.set_resizable(resize);
        // The extension only prevents interactive resizing; button flags and
        // programmatic setSize/setInnerSize remain independent.
        self.window.set_enabled_buttons(buttons);
        if self.window.is_resizable() != resize {
            return Err("window manager did not apply resize permission".into());
        }
        self.disable_resize = disabled;
        Ok(())
    }

    pub(super) fn z_order(
        &self,
        order: ZOrder,
        activate: bool,
        levels: bool,
    ) -> Result<(), String> {
        if activate {
            // focus_window also activates an inactive application, unlike
            // windowEx's SetWindowPos. Do not substitute that stronger action.
            return Err(
                "changing window order with application-local activation is unavailable".into(),
            );
        }
        if !levels {
            return Err("window stacking is unavailable on this backend".into());
        }
        let level = match order {
            ZOrder::Topmost => WindowLevel::AlwaysOnTop,
            ZOrder::NotTopmost => WindowLevel::Normal,
            // AlwaysOnBottom is a persistent level, not the one-time lowering
            // performed by sendToBack; winit does not expose that operation.
            ZOrder::Top | ZOrder::Bottom | ZOrder::Behind(_) => {
                return Err("relative window stacking is unavailable on this backend".into());
            }
        };
        self.window.set_window_level(level);
        Ok(())
    }

    pub(super) fn start_control(&mut self, request: Request, action: Control) {
        if self.control.is_some() || self.resize.is_some() {
            request.respond(Err("another window geometry operation is pending".into()));
            return;
        }
        let before = self.snapshot();
        let action = match action {
            Control::Maximized(value) if value == before.maximized => {
                request.respond(Ok(Response::Snapshot(before)));
                return;
            }
            Control::Minimized(value) if Some(value) == before.minimized => {
                request.respond(Ok(Response::Snapshot(before)));
                return;
            }
            Control::Maximized(true) => Control::Maximize,
            Control::Maximized(false) | Control::Minimized(false) => Control::Restore,
            Control::Minimized(true) => Control::Minimize,
            action => action,
        };
        let minimized = before.minimized;
        if (matches!(action, Control::Minimize)
            || (matches!(action, Control::Restore) && !before.maximized))
            && minimized.is_none()
        {
            request.respond(Err(
                "minimized window state cannot be confirmed on this backend".into(),
            ));
            return;
        }
        let (want_min, want_max) = match action {
            Control::Minimize => (Some(true), false),
            Control::Maximize => (minimized.map(|_| false), true),
            Control::Restore if minimized == Some(true) => (Some(false), self.restore_maximized),
            Control::Restore => (minimized.map(|_| false), false),
            _ => unreachable!(),
        };
        if matches!(action, Control::Maximize)
            && !matches!(
                self.border_style,
                krkr_protocol::window::BorderStyle::Sizeable
                    | krkr_protocol::window::BorderStyle::SizeToolWin
            )
        {
            // Upstream SC_MAXIMIZE does nothing for fixed-size windows.
            request.respond(Ok(Response::Snapshot(before)));
            return;
        }
        // SC_RESTORE also displays a hidden normal window. Update both the
        // native renderer's visibility and the shared record via the snapshot.
        self.window.set_visible(true);
        self.visible = true;
        self.window.request_redraw();
        if want_min == minimized && want_max == before.maximized {
            request.respond(Ok(Response::Snapshot(self.snapshot())));
            return;
        }
        let now = Instant::now();
        let needs_resize = want_min != Some(true) && want_max != before.maximized;
        self.control = Some(PendingControl {
            request,
            minimized: want_min,
            maximized: want_max,
            needs_resize,
            resized: false,
            earliest: now + Duration::from_millis(10),
            deadline: now + Duration::from_secs(2),
        });
        match action {
            Control::Minimize => self.window.set_minimized(true),
            Control::Maximize => {
                if minimized == Some(true) {
                    self.window.set_minimized(false);
                }
                self.window.set_maximized(true);
            }
            Control::Restore if minimized == Some(true) => {
                self.window.set_minimized(false);
                self.window.set_maximized(want_max);
            }
            Control::Restore => self.window.set_maximized(false),
            _ => unreachable!(),
        }
    }

    /// Called after native events, never completing from requested flags alone.
    pub(super) fn poll_control(&mut self) -> bool {
        let Some(pending) = &self.control else {
            return false;
        };
        if pending.request.cancelled() {
            self.control = None;
            return false;
        }
        let now = Instant::now();
        let maximized = self.window.is_minimized() != Some(true) && self.window.is_maximized();
        let confirmed = now >= pending.earliest
            && maximized == pending.maximized
            && (pending.minimized.is_none() || self.window.is_minimized() == pending.minimized)
            && (!pending.needs_resize || pending.resized);
        if confirmed || now >= pending.deadline {
            let pending = self.control.take().expect("pending window control");
            if confirmed {
                pending
                    .request
                    .respond(Ok(Response::Snapshot(self.snapshot())));
            } else {
                pending.request.respond(Err(
                    "window manager did not confirm the requested state within two seconds".into(),
                ));
            }
            false
        } else {
            true
        }
    }
}
