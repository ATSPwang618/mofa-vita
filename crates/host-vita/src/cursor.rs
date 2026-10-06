use krkr_protocol::{
    graphics::{Rect, Size},
    input_style::CursorImage,
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Gpu, Image};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

#[derive(Default)]
pub(crate) struct Cursors {
    entries: HashMap<i32, Weak<CursorImage>>,
    texture: Option<(usize, Image)>,
}
impl Cursors {
    pub fn register(
        &mut self,
        id: i32,
        image: &Arc<CursorImage>,
        limit: usize,
    ) -> Result<(), String> {
        self.entries.retain(|_, image| image.strong_count() != 0);
        if !self.entries.contains_key(&id) && self.entries.len() >= limit {
            return Err("cursor capacity reached".into());
        }
        if image.width == 0
            || image.height == 0
            || image.rgba.as_slice().len()
                != usize::from(image.width) * usize::from(image.height) * 4
        {
            return Err("invalid cursor image".into());
        }
        self.entries.insert(id, Arc::downgrade(image));
        Ok(())
    }
    pub fn contains(&self, id: i32) -> bool {
        id < 2
            || self
                .entries
                .get(&id)
                .is_some_and(|image| image.strong_count() != 0)
    }
    pub fn draw(&mut self, gpu: &Gpu, id: i32, position: (i32, i32)) -> Result<(), String> {
        let custom = if id >= 2 {
            Some(
                self.entries
                    .get(&id)
                    .and_then(Weak::upgrade)
                    .ok_or("cursor image has been released")?,
            )
        } else {
            None
        };
        let identity = custom
            .as_ref()
            .map_or(0, |image| Arc::as_ptr(image) as usize);
        let hotspot = custom.as_ref().map_or((0, 0), |image| image.hotspot);
        if self
            .texture
            .as_ref()
            .is_none_or(|(old, _)| *old != identity)
        {
            self.texture = None;
            let size = custom.as_ref().map_or(
                Size {
                    width: 12,
                    height: 18,
                },
                |image| Size {
                    width: image.width.into(),
                    height: image.height.into(),
                },
            );
            let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging)
                .map_err(|e| e.to_string())?;
            if let Some(image) = custom {
                bytes.as_mut_slice().copy_from_slice(image.rgba.as_slice());
            } else {
                // The one built-in pointer asset; all game cursors retain their
                // original pixels and hotspot. No platform font/icon discovery.
                const ARROW: [&[u8]; 18] = [
                    b"#...........",
                    b"##..........",
                    b"#o#.........",
                    b"#oo#........",
                    b"#ooo#.......",
                    b"#oooo#......",
                    b"#ooooo#.....",
                    b"#oooooo#....",
                    b"#ooooooo#...",
                    b"#oooooooo#..",
                    b"#ooooooooo#.",
                    b"#oooooo#####",
                    b"#ooo#oo#....",
                    b"#oo#.#oo#...",
                    b"#o#..#oo#...",
                    b"##....#oo#..",
                    b"#.....#oo#..",
                    b".......##...",
                ];
                for (pixel, &symbol) in bytes
                    .as_mut_slice()
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(ARROW.into_iter().flatten())
                {
                    pixel.copy_from_slice(match symbol {
                        b'#' => &[0, 0, 0, 255],
                        b'o' => &[255, 255, 255, 255],
                        _ => &[0; 4],
                    });
                }
            }
            let mut image = gpu
                .reserve_upload(size, true, false)
                .map_err(|e| e.to_string())?;
            gpu.upload(
                &mut image,
                &Pixels {
                    size,
                    main: Some(bytes),
                    province: None,
                },
            )
            .map_err(|e| e.to_string())?;
            self.texture = Some((identity, image));
        }
        let image = &self.texture.as_ref().unwrap().1;
        gpu.present_cursor(
            image,
            crate::window::DISPLAY,
            Rect {
                left: position.0.saturating_sub(hotspot.0.into()),
                top: position.1.saturating_sub(hotspot.1.into()),
                ..image.size.rect()
            },
        )
        .map_err(|e| e.to_string())
    }
}
