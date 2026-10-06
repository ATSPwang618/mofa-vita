//! E-mote RL, indexed images and RGBA8 atlas extraction. Output is straight
//! RGBA for the shared graphics protocol, with all allocations budgeted.
use super::resource::{File, Icon};
use krkr_engine::protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use tjs_core::{NativeError, NativeResult};

fn extent(v: f64) -> NativeResult<u32> {
    if !v.is_finite() || v < 1. || v > i32::MAX as f64 {
        return Err(NativeError::Message("invalid E-mote image extent"));
    }
    Ok(v as u32)
}
fn checked(stop: &dyn Fn() -> bool) -> NativeResult<()> {
    if stop() {
        Err(NativeError::Message("E-mote image decode cancelled"))
    } else {
        Ok(())
    }
}
fn palette_color(palette: &[u8], index: u8) -> [u8; 4] {
    let offset = usize::from(index) * 4;
    palette
        .get(offset..offset + 4)
        .map_or([0; 4], |p| [p[0], p[1], p[2], p[3]])
}
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> NativeResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(NativeError::Message("E-mote image offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(NativeError::Message("truncated E-mote RL image"))?;
        self.position = end;
        Ok(bytes)
    }
}
fn rl(
    input: &[u8],
    output: &mut [u8],
    palette: Option<&[u8]>,
    stop: &dyn Fn() -> bool,
) -> NativeResult<()> {
    let mut reader = Reader {
        bytes: input,
        position: 0,
    };
    let mut position = 0;
    while reader.position < input.len() && position < output.len() {
        checked(stop)?;
        let token = reader.take(1)?[0];
        let repeat = token & 0x80 != 0;
        let count = if repeat {
            usize::from(token & 0x7f) + 3
        } else {
            usize::from(token) + 1
        };
        let count = count.min((output.len() - position) / 4);
        let target = &mut output[position..position + count * 4];
        if repeat {
            let color = if let Some(palette) = palette {
                palette_color(palette, reader.take(1)?[0])
            } else {
                let p = reader.take(4)?;
                [p[0], p[1], p[2], p[3]]
            };
            for pixel in target.as_chunks_mut::<4>().0.iter_mut() {
                pixel.copy_from_slice(&color);
            }
        } else if let Some(palette) = palette {
            for (pixel, &index) in target
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(reader.take(count)?)
            {
                pixel.copy_from_slice(&palette_color(palette, index));
            }
        } else {
            target.copy_from_slice(reader.take(count * 4)?);
        }
        position += count * 4;
    }
    Ok(())
}
impl File {
    pub fn decode_icon(
        &self,
        icon: &Icon,
        budget: &Budget,
        stop: &dyn Fn() -> bool,
    ) -> NativeResult<Pixels> {
        checked(stop)?;
        let size = Size {
            width: extent(icon.size[0])?,
            height: extent(icon.size[1])?,
        };
        let length = size
            .rgba_bytes()
            .ok_or(NativeError::Message("E-mote image size overflow"))?;
        let mut main =
            Bytes::zeroed(length, budget).map_err(|e| NativeError::Detail(e.to_string()))?;
        let Some(range) = &icon.pixels else {
            return Ok(Pixels {
                size,
                main: Some(main),
                province: None,
            });
        };
        let input = self.data(range).map_err(NativeError::Message)?;
        let palette = icon
            .palette
            .as_ref()
            .map(|r| self.data(r))
            .transpose()
            .map_err(NativeError::Message)?;
        let output = main.as_mut_slice();
        match icon.compression.as_str() {
            "RL" => rl(input, output, palette, stop)?,
            "none" => {
                if let Some(palette) = palette {
                    for (i, (pixel, &index)) in output
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(input)
                        .enumerate()
                    {
                        if i % 16384 == 0 {
                            checked(stop)?;
                        }
                        pixel.copy_from_slice(&palette_color(palette, index));
                    }
                } else {
                    let length = input.len().min(output.len());
                    for (to, from) in output[..length]
                        .chunks_mut(65536)
                        .zip(input[..length].chunks(65536))
                    {
                        checked(stop)?;
                        to.copy_from_slice(from);
                    }
                }
            }
            _ if !self.krkr && icon.format == "RGBA8" => {
                let width = extent(icon.texture_size[0])? as usize;
                let height = extent(icon.texture_size[1])? as usize;
                if !icon
                    .position
                    .iter()
                    .all(|v| v.is_finite() && *v >= 0. && *v <= i32::MAX as f64)
                {
                    return Err(NativeError::Message("invalid E-mote atlas position"));
                }
                let x = icon.position[0] as usize;
                let y = icon.position[1] as usize;
                let columns = (size.width as usize).min(width.saturating_sub(x));
                let rows = (size.height as usize).min(height.saturating_sub(y));
                if columns > 0 {
                    let pitch = width
                        .checked_mul(4)
                        .ok_or(NativeError::Message("E-mote atlas stride overflow"))?;
                    for row in 0..rows {
                        checked(stop)?;
                        let start = (y + row)
                            .checked_mul(pitch)
                            .and_then(|n| n.checked_add(x * 4))
                            .ok_or(NativeError::Message("E-mote atlas offset overflow"))?;
                        let end = start
                            .checked_add(columns * 4)
                            .ok_or(NativeError::Message("E-mote atlas offset overflow"))?;
                        let source = input
                            .get(start..end)
                            .ok_or(NativeError::Message("truncated E-mote atlas"))?;
                        let target = row * size.width as usize * 4;
                        output[target..target + source.len()].copy_from_slice(source);
                    }
                }
            }
            // ensureLoad in the reference retains its zeroed buffer when the
            // decoder does not recognize a format; it remains transparent.
            _ => {}
        }
        if !self.rgba {
            for row in output.chunks_mut(65536) {
                checked(stop)?;
                for pixel in row.as_chunks_mut::<4>().0.iter_mut() {
                    pixel.swap(0, 2);
                }
            }
        }
        Ok(Pixels {
            size,
            main: Some(main),
            province: None,
        })
    }
}
