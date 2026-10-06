//! Vita host: the VM worker uses the portable engine; the main thread owns
//! PVR/EGL, ordered graphics, script windows, controller and touch presentation.
#[cfg(target_os = "vita")]
pub mod audio;
#[cfg(target_os = "vita")]
mod audio_at9;
#[cfg(any(target_os = "vita", test))]
mod audio_prefetch;
pub mod bootstrap;
mod cursor;
#[cfg(target_os = "vita")]
pub mod files;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[allow(unsafe_code, dead_code)]
#[path = "../../render-gles2/tests/support/mod.rs"]
mod gles_test_support;
pub mod graphics;
pub mod input;
pub mod launcher;
mod logging;
pub mod memory;
mod overlay;
#[cfg(target_os = "vita")]
pub mod power;
#[cfg(target_os = "vita")]
pub mod pvr;
pub mod system;
#[cfg(target_os = "vita")]
pub mod time;
#[cfg(target_os = "vita")]
pub mod video;
pub mod wake;
pub mod watchdog;
pub mod window;

pub fn create_engine(
    runtime: tjs_runtime::Runtime,
    config: krkr_engine::system::SystemConfig,
) -> Result<krkr_engine::Engine<tjs_runtime::clock::MonotonicClock>, String> {
    create_engine_with_clock(
        runtime,
        config,
        tjs_runtime::clock::MonotonicClock::default(),
    )
}

pub fn create_engine_with_clock<C: tjs_runtime::clock::Clock + 'static>(
    runtime: tjs_runtime::Runtime,
    mut config: krkr_engine::system::SystemConfig,
    clock: C,
) -> Result<krkr_engine::Engine<C>, String> {
    #[cfg(target_os = "vita")]
    time::install();
    if config.host.is_none() {
        config.host = Some(Box::<system::VitaSystem>::default());
    }
    let mut engine = krkr_engine::Engine::with_system(
        runtime,
        clock,
        Default::default(),
        Default::default(),
        config,
    )
    .map_err(|e| e.to_string())?;
    engine
        .set_bitmap_memory_limit(memory::BITMAP_BYTES)
        .map_err(|e| e.to_string())?;
    engine
        .set_font_memory_limit(memory::FONT_BYTES)
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "vita")]
    engine
        .set_audio_output(audio::Audio::default())
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "vita")]
    engine.set_video_backend(video::Backend::default());
    #[cfg(target_os = "vita")]
    engine.set_audio_decoder_backend(audio_at9::Backend);
    Ok(engine)
}
