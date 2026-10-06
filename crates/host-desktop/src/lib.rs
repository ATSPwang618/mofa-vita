//! Desktop dependency assembly. Platform libraries stay outside the engine and
//! language crates; the Vita host can supply the same service contracts.
pub mod audio;
pub mod clipboard;
pub mod font;
pub mod memory;
pub mod system;
pub mod window;
pub use krkr_video_ffmpeg::Ffmpeg as Video;
pub use krkr_video_ffmpeg::WaveBackend as AudioDecoder;

/// Assemble PC services around the portable engine. Headless PC tools omit
/// the window endpoint but retain filesystem, font and media capabilities.
/// Game policy and static plugin selection belong to the caller.
pub fn create_engine(
    runtime: tjs_runtime::Runtime,
    mut config: krkr_engine::system::SystemConfig,
    windows: Option<krkr_protocol::window::Client>,
) -> Result<krkr_engine::Engine<tjs_runtime::clock::MonotonicClock>, String> {
    system::configure(&mut config)?;
    let data =
        krkr_engine::assets::local::from_storage(&config.data_path).map_err(|e| e.to_string())?;
    let data = krkr_engine::assets::local::resolve(&data).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data)
        .map_err(|e| format!("cannot prepare data directory {}: {e}", data.display()))?;
    let mut engine = krkr_engine::Engine::with_system(
        runtime,
        tjs_runtime::clock::MonotonicClock::default(),
        Default::default(),
        Default::default(),
        config,
    )
    .map_err(|e| e.to_string())?;
    engine
        .set_bitmap_memory_limit(memory::BITMAP_BYTES)
        .map_err(|e| e.to_string())?;
    krkr_engine::clipboard::set_host(
        &mut engine.runtime_mut().heap,
        clipboard::SystemClipboard::default(),
    )
    .map_err(|e| e.to_string())?;
    engine.set_font_provider(font::Fonts::discover().map_err(|e| e.to_string())?);
    engine
        .set_font_memory_limit(memory::FONT_BYTES)
        .map_err(|e| e.to_string())?;
    engine.set_video_backend(Video);
    engine.set_audio_decoder_backend(AudioDecoder);
    engine
        .set_audio_output(audio::Audio::default())
        .map_err(|e| e.to_string())?;
    if let Some(host) = windows {
        engine.attach_windows(host).map_err(|e| e.to_string())?;
    }
    Ok(engine)
}
