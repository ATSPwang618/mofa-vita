//! Executable, VM-free transition instances. A backend advertises and registers
//! kernels by versioned identity; instances supply owned, budgeted frame data.
//! Neither shaders nor script values cross the engine/host protocol.
use crate::{budget::Budget, graphics::Size, pixels::Bytes};
use std::sync::Arc;

pub trait Instance: std::fmt::Debug + Send + Sync {
    fn kernel(&self) -> &'static str;
    /// Runs on the render host. Tables may be immutable across frames or local
    /// to one frame. The backend must copy or retain their data before returning.
    fn prepare(
        &self,
        size: Size,
        elapsed: u64,
        duration: u64,
        budget: &Budget,
    ) -> Result<Payload, String>;
}
pub struct Payload {
    pub parameters: [u32; 16],
    pub table: Arc<Bytes>,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub instance: Arc<dyn Instance>,
    pub elapsed: u64,
    pub duration: u64,
    /// Pins the provider while queued scenes or submissions still execute it.
    pub lifetime: Arc<()>,
}
