//! Vita game library, preferences and startup-file browser powered by Nivora.
#![cfg_attr(not(target_os = "vita"), allow(dead_code))]
mod browser;
mod catalog;
mod input;
mod render;
mod ui;
pub(crate) use catalog::valid_startup;
pub use catalog::{Catalog, CursorSpeed, Game, Language, RenderQuality};

pub const ROOT: &str = "ux0:/data/KRKR";
pub struct Selection {
    pub directory: std::path::PathBuf,
    pub cursor_speed: f32,
    pub script_logs: bool,
    pub engine_logs: bool,
    pub show_stats: bool,
    pub startup: String,
    pub language: Language,
    pub render_quality: RenderQuality,
}

#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
pub fn choose(
    display: &crate::pvr::PvrContext,
    controller: &mut crate::input::native::Controller,
    error: &str,
) -> Result<Option<Selection>, String> {
    run(
        &display.glow(),
        || controller.sample(),
        || display.swap(),
        std::path::Path::new(ROOT),
        error,
    )
}

#[allow(unsafe_code)]
fn run(
    gl: &glow::Context,
    mut sample: impl FnMut() -> Result<crate::input::Sample, String>,
    mut swap: impl FnMut() -> Result<(), String>,
    root: &std::path::Path,
    error: &str,
) -> Result<Option<Selection>, String> {
    use input::Event;
    use nivora_render_gles2::Gles2Target;
    use std::time::{Duration, Instant};
    use ui::{Effect, LauncherUi, VIEWPORT};
    // Renderer and target are dropped before the host releases the current context.
    let target = unsafe { Gles2Target::new(gl) };
    let mut renderer = render::LauncherRenderer::new(&target, gl)?;
    let mut app = LauncherUi::new(Catalog::empty(root)).map_err(|e| e.to_string())?;
    if let Err(error) = app.load(root.into()) {
        app.show_error(&error).map_err(|e| e.to_string())?;
    }
    let mut pending_error = (!error.is_empty()).then(|| error.to_owned());
    let mut input = input::Input::new(sample()?);
    let mut previous = Instant::now();
    let mut dirty = true;
    loop {
        let now = Instant::now();
        dirty |= app.advance(now.saturating_duration_since(previous));
        previous = now;
        let was_busy = app.busy();
        match app.poll() {
            Ok(changed) => dirty |= changed,
            Err(error) => {
                app.show_error(&error).map_err(|e| e.to_string())?;
                dirty = true;
            }
        }
        if was_busy && !app.busy() {
            input = input::Input::new(sample()?);
        }
        if !app.busy()
            && let Some(error) = pending_error.take()
        {
            app.show_error(&error).map_err(|e| e.to_string())?;
            dirty = true;
        }
        app.layout(renderer.measurer()).map_err(|e| e.to_string())?;
        for event in input.poll(sample()?, now) {
            dirty = true;
            let result = match event {
                Event::Ui(event) => app.handle(event, renderer.measurer()),
                Event::Shortcut(action) => app.shortcut(action),
                Event::CancelPointer => {
                    app.cancel_pointer();
                    Ok(None)
                }
            };
            match result {
                Ok(Some(Effect::Launch(selection))) => {
                    let mut page = app.loading_page().map_err(|e| e.to_string())?;
                    page.layout(VIEWPORT, renderer.measurer())
                        .map_err(|e| e.to_string())?;
                    renderer.present(&page.frame())?;
                    swap()?;
                    return Ok(Some(selection));
                }
                Ok(Some(Effect::Exit)) => return Ok(None),
                Ok(None) => {}
                Err(error) => app.show_error(&error).map_err(|e| e.to_string())?,
            }
            app.layout(renderer.measurer()).map_err(|e| e.to_string())?;
        }
        if dirty {
            renderer.present(&app.frame())?;
            swap()?;
            dirty = false;
        } else {
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}
