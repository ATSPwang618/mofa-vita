//! A missing plane, a constant plane and sampled alpha are distinct states.
use crate::{
    budget::{Budget, BudgetError},
    graphics::Size,
    pixels::Bytes,
};

#[derive(Debug)]
pub enum Data {
    Empty,
    Uniform(u8),
    Samples(Bytes),
    /// One byte per stored pixel; logical hit coordinates retain pixel-center
    /// nearest sampling, just like rendering a compact image.
    Scaled {
        size: Size,
        samples: Bytes,
    },
}
#[derive(Debug)]
pub struct Plane {
    pub size: Size,
    pub data: Data,
}
impl Plane {
    pub fn sample(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.size.width.into() || y >= self.size.height.into() {
            return 0;
        }
        match &self.data {
            Data::Empty => 0,
            Data::Uniform(value) => *value,
            Data::Samples(bytes) => {
                bytes.as_slice()[y as usize * self.size.width as usize + x as usize]
            }
            Data::Scaled { size, samples } => {
                let sx = ((2 * x as u64 + 1) * u64::from(size.width)
                    / (2 * u64::from(self.size.width))) as usize;
                let sy = ((2 * y as u64 + 1) * u64::from(size.height)
                    / (2 * u64::from(self.size.height))) as usize;
                samples.as_slice()[sy * size.width as usize + sx]
            }
        }
    }
    pub fn bytes(&self) -> usize {
        match &self.data {
            Data::Samples(bytes) => bytes.as_slice().len(),
            Data::Scaled { samples, .. } => samples.as_slice().len(),
            _ => 0,
        }
    }
    /// RGBA readbacks retain only alpha; province readbacks already have one
    /// byte per sample. Uniform planes need no resident pixel allocation.
    pub fn from_pixels(
        size: Size,
        source: Bytes,
        channels: usize,
        budget: &Budget,
    ) -> Result<Self, BudgetError> {
        let pixels = source.as_slice();
        let first = pixels[channels - 1];
        let data = if pixels
            .chunks_exact(channels)
            .all(|p| p[channels - 1] == first)
        {
            Data::Uniform(first)
        } else if channels == 1 {
            Data::Samples(source)
        } else {
            let (mut data, source_permit) = source.into_parts();
            let length = data.len() / channels;
            for index in 0..length {
                data[index] = data[index * channels + channels - 1];
            }
            data.truncate(length);
            data.shrink_to_fit();
            let permit = budget.reserve(data.capacity())?;
            drop(source_permit);
            Data::Samples(Bytes::with_permit(data, permit))
        };
        Ok(Self { size, data })
    }
    pub fn from_scaled_pixels(
        logical: Size,
        stored: Size,
        source: Bytes,
        channels: usize,
        budget: &Budget,
    ) -> Result<Self, BudgetError> {
        let plane = Self::from_pixels(stored, source, channels, budget)?;
        Ok(Self {
            size: logical,
            data: match plane.data {
                Data::Samples(samples) if stored != logical => Data::Scaled {
                    size: stored,
                    samples,
                },
                data => data,
            },
        })
    }
}
