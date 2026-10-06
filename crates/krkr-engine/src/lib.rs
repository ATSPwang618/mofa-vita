//! Engine script interfaces. Platform IO is injected; the language crates do
//! not know about engine classes, windows or filesystem implementations.
mod async_trigger;
mod bitmap;
pub mod clipboard;
mod color;
pub mod debug;
pub mod engine;
mod events;
pub mod extensions;
mod io;
pub mod kag;
mod operations;
pub mod plugins;
pub mod scripts;
pub mod storages;
pub mod system;
pub use krkr_assets as assets;
mod font;
mod layer;
mod menu;
mod rect;
mod sound;
mod video;
pub use krkr_video as media;
mod timer;
mod timer_queue;
mod window;
pub use engine::{Engine, EngineEvent, SystemEvent};
pub use krkr_audio as audio;
pub use krkr_protocol as protocol;
pub use timer_queue::TimerLimits;

/// Apply the engine environment before compiling its startup script. This
/// matches krkrz ScriptMgnIntf.cpp; plain TJS execution has no engine flag.
pub fn configure_preprocessor(preprocessor: &mut tjs_front::Preprocessor) {
    preprocessor.set_default("kirikiriz", 1);
}

pub fn install(
    runtime: &mut tjs_runtime::Runtime,
    output: impl debug::LogOutput + 'static,
) -> tjs_core::NativeResult<()> {
    scripts::install(&mut runtime.heap)?;
    kag::install(&mut runtime.heap)?;
    debug::install(&mut runtime.heap, output)?;
    Ok(())
}
