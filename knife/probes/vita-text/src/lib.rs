//! A bounded probe of the game's original font atlas and layout on the Vita facade.
//! Host checks record native calls; they do not execute shaders or prove visible text.

#[cfg(all(test, not(target_os = "vita")))]
extern crate self as knife_render;
#[cfg(all(test, not(target_os = "vita")))]
include!(concat!(env!("OUT_DIR"), "/vita_facade_module.rs"));

// These are the only game-global queries used by the original text module.
// Both reflect the actual Witch build selected by the compact game project.
#[cfg(any(test, target_os = "vita"))]
mod assets {
    pub fn witch() -> bool { true }
    pub fn overseas() -> bool { false }
}

#[cfg(any(test, target_os = "vita"))]
#[path = "../../../knife/src/text.rs"]
pub mod text;

#[cfg(any(test, target_os = "vita"))]
mod probe;
#[cfg(any(test, target_os = "vita"))]
pub use probe::{FontInputs, Language, PreparedPage, prepare_page, run_text_probe};

#[cfg(all(test, not(target_os = "vita")))]
mod tests;
