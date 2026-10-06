//! Reserved storage cannot be sampled before a complete upload initializes it.
use crate::{Error, Gpu, Image, Result};
use krkr_protocol::{graphics::Size, pixels::Pixels};

pub struct PendingUpload {
    pub(crate) image: Image,
    pub(crate) main: bool,
    pub(crate) province: bool,
}
impl PendingUpload {
    /// Consuming the reservation keeps failed or partial transfers private.
    /// Dropping it on cancellation retires its allocations normally.
    pub fn complete(mut self, gpu: &Gpu, pixels: &Pixels, logical: Option<Size>) -> Result<Image> {
        if pixels.size != self.image.size
            || pixels.main.is_some() != self.main
            || pixels.province.is_some() != self.province
        {
            return Err(Error::Message(
                "upload differs from its private reservation",
            ));
        }
        if let Some(size) = logical
            && (!self.main
                || self.province
                || size.width == 0
                || size.height == 0
                || size.width > i32::MAX as u32
                || size.height > i32::MAX as u32)
        {
            return Err(Error::Message("invalid compact upload reservation"));
        }
        gpu.upload(&mut self.image, pixels)?;
        match logical {
            Some(size) => gpu.logical_image(self.image, size),
            None => Ok(self.image),
        }
    }
}
