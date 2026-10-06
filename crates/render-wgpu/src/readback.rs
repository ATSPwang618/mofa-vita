use crate::gpu::{Gpu, Image, extent};
use krkr_protocol::graphics::{Rect, Size};
use krkr_protocol::pixels::Bytes;
use krkr_render::{Error, Result, budget::Permit};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};

pub struct Pixels {
    pub data: Bytes,
    pub size: Size,
    pub channels: usize,
}
pub struct Readback {
    buffer: wgpu::Buffer,
    ready: Option<Receiver<std::result::Result<(), wgpu::BufferAsyncError>>>,
    size: Size,
    channels: usize,
    stride: usize,
    _permit: Arc<Permit>,
    data: Option<Bytes>,
}
impl Readback {
    /// Poll the device separately; taking a result never blocks the UI thread.
    pub fn take(&mut self) -> Option<Result<Pixels>> {
        match self.ready.as_ref()?.try_recv() {
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.ready = None;
                return Some(Err(Error::Message("GPU readback disconnected")));
            }
            Ok(Err(error)) => {
                self.ready = None;
                return Some(Err(Error::Backend(error.to_string())));
            }
            Ok(Ok(())) => {}
        }
        self.ready = None;
        let row_bytes = self.size.width as usize * self.channels;
        let mut data = self.data.take().expect("readback pixels");
        {
            let mapping = match self.buffer.slice(..).get_mapped_range() {
                Ok(mapping) => mapping,
                Err(error) => return Some(Err(Error::Backend(error.to_string()))),
            };
            for (row, output) in mapping
                .chunks_exact(self.stride)
                .zip(data.as_mut_slice().chunks_exact_mut(row_bytes))
            {
                output.copy_from_slice(&row[..row_bytes]);
            }
        }
        self.buffer.unmap();
        Some(Ok(Pixels {
            data,
            size: self.size,
            channels: self.channels,
        }))
    }
}
impl Gpu {
    pub fn readback(&self, image: &Image, rectangle: Rect, province: bool) -> Result<Readback> {
        if image.size.rect().intersection(rectangle) != Some(rectangle) {
            return Err(Error::Message("pixel read is outside the image"));
        }
        let size = Size {
            width: rectangle.width,
            height: rectangle.height,
        };
        let channels = if province { 1 } else { 4 };
        let row_bytes = size.width as usize * channels;
        let stride = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let bytes = stride
            .checked_mul(size.height as usize)
            .ok_or(Error::Message("readback byte size overflow"))?;
        let permit = Arc::new(self.staging.reserve(bytes)?);
        let data = Bytes::zeroed(row_bytes * size.height as usize, &self.staging)?;
        // Admit the caller's complete CPU result before generating GPU pixels.
        // Large logical reads retain the same staging bound as ordinary reads.
        let resolved = if province {
            None
        } else {
            Some(self.resolve_main(image, rectangle)?)
        };
        let rectangle = if let Some(region) = &resolved {
            Rect {
                left: rectangle.left - region.rectangle.left,
                top: rectangle.top - region.rectangle.top,
                ..rectangle
            }
        } else {
            rectangle
        };
        let target = if province {
            image
                .province
                .as_ref()
                .ok_or(Error::Message("image has no province plane"))?
        } else {
            &resolved.as_ref().unwrap().allocation
        };
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image readback"),
            size: bytes as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rectangle.left as u32,
                    y: rectangle.top as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: None,
                },
            },
            extent(size),
        );
        self.submit(encoder, vec![target.clone()]);
        let (send, ready) = mpsc::sync_channel(1);
        let pin = permit.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
                drop(pin);
            });
        self.check()?;
        Ok(Readback {
            buffer,
            ready: Some(ready),
            size,
            channels,
            stride,
            _permit: permit,
            data: Some(data),
        })
    }
}
