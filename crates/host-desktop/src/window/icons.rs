//! Windows has separate small/title and large/taskbar icons. X11 has one icon:
//! the application selection supplies a default for windows without an override.
//! winit does not implement runtime icons on Wayland or macOS; report that fact.
use super::*;
use krkr_protocol::{
    budget::Permit,
    window::{IconCommand, IconImage},
};
use winit::window::Icon;

pub(super) struct NativeIcon {
    icon: Icon,
    source: Arc<IconImage>,
    // Retained for as long as either a window or the application uses this
    // native resource, including after the script replaces its selected image.
    _native_memory: Permit,
}
fn supported(event_loop: &ActiveEventLoop) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::ActiveEventLoopExtX11;
        event_loop.is_x11()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = event_loop;
        cfg!(windows)
    }
}
impl<F, T> App<F, T> {
    fn native_icon(
        &self,
        image: &Option<Arc<IconImage>>,
        window: Option<NativeId>,
    ) -> Result<Option<Arc<NativeIcon>>, String> {
        let Some(image) = image else {
            return Ok(None);
        };
        for existing in [
            window
                .and_then(|id| self.windows.get(&id))
                .and_then(|native| native.icon.as_ref()),
            self.application_icon.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if Arc::ptr_eq(&existing.source, image) {
                return Ok(Some(existing.clone()));
            }
        }
        if image.width == 0
            || image.height == 0
            || image.width > 256
            || image.height > 256
            || image.rgba.as_slice().len() != (image.width * image.height * 4) as usize
        {
            return Err("invalid window icon pixels".into());
        }
        // RGBA conversion input plus native color/mask storage. The original
        // decoded buffer already carries its own permit from the same pool.
        let memory = self
            .host
            .staging_budget()
            .reserve(image.rgba.as_slice().len() * 3)
            .map_err(|error| error.to_string())?;
        let icon = Icon::from_rgba(image.rgba.as_slice().to_vec(), image.width, image.height)
            .map_err(|error| error.to_string())?;
        Ok(Some(Arc::new(NativeIcon {
            icon,
            source: image.clone(),
            _native_memory: memory,
        })))
    }
    pub(super) fn apply_icons(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: WindowId,
        command: &IconCommand,
    ) -> Result<(), String> {
        if !supported(event_loop) {
            return Err("runtime window/application icons are unavailable on this backend".into());
        }
        match command {
            IconCommand::Window {
                image,
                with_application,
            } => {
                let id = *self.ids.get(&window).ok_or("window is closed")?;
                if !self.windows.get(&id).is_some_and(Native::alive) {
                    return Err("window is closed".into());
                }
                let icon = self.native_icon(image, Some(id))?;
                if *with_application {
                    self.apply_application_icon(icon.clone());
                }
                let native = self.windows.get_mut(&id).ok_or("window is closed")?;
                native.apply_window_icon(icon.as_ref(), self.application_icon.as_ref());
                native.icon = icon;
            }
            IconCommand::Application(image) => {
                let icon = self.native_icon(image, None)?;
                self.apply_application_icon(icon);
            }
        }
        Ok(())
    }
    fn apply_application_icon(&mut self, icon: Option<Arc<NativeIcon>>) {
        for native in self.windows.values() {
            native.apply_application_icon(icon.as_ref());
        }
        self.application_icon = icon;
    }
}
impl Native {
    pub(super) fn initialize_icons(&self, application: Option<&Arc<NativeIcon>>) {
        self.apply_application_icon(application);
    }
    fn apply_window_icon(
        &self,
        icon: Option<&Arc<NativeIcon>>,
        application: Option<&Arc<NativeIcon>>,
    ) {
        #[cfg(windows)]
        let selected = {
            let _ = application;
            icon
        };
        #[cfg(not(windows))]
        let selected = icon.or(application);
        self.window
            .set_window_icon(selected.map(|image| image.icon.clone()));
    }
    fn apply_application_icon(&self, icon: Option<&Arc<NativeIcon>>) {
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowExtWindows;
            self.window
                .set_taskbar_icon(icon.map(|image| image.icon.clone()));
        }
        #[cfg(target_os = "linux")]
        if self.icon.is_none() {
            self.apply_window_icon(None, icon);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = icon;
    }
}
