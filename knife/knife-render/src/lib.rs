//! Renderer facade. Script layout and public draw types are shared across backends.
//! Desktop uses WGPU; Vita records the same operations for the native GXM backend.

#[cfg(not(target_os = "vita"))]
mod desktop;
#[cfg(not(target_os = "vita"))]
pub use desktop::*;

#[cfg(any(target_os = "vita", test))]
mod vita;
#[cfg(target_os = "vita")]
pub use vita::*;

mod byte_view;
pub use byte_view::{ByteView, PixelOwner, PixelView};

pub mod target_ring;
