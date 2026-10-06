//! Platform clipboard access is lazy: constructing this service never reads,
//! writes, clears, or starts monitoring the user's clipboard.
use clipboard_rs::{
    Clipboard, ClipboardContent, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, ContentFormat, RustImageData, WatcherShutdown, common::RustImage,
};
use krkr_engine::clipboard::{DATA_LIMIT, Data, Host, LAYER_FORMAT, TJS_FORMAT, Watch};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
pub struct SystemClipboard(Option<ClipboardContext>);
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn length(size: Size) -> Result<usize, String> {
    if size.width == 0 || size.height == 0 || size.width > 16384 || size.height > 16384 {
        return Err("invalid clipboard image dimensions".into());
    }
    size.rgba_bytes()
        .filter(|&n| n <= DATA_LIMIT)
        .ok_or_else(|| "clipboard image exceeds size limit".into())
}
impl SystemClipboard {
    fn context(&mut self) -> Result<&ClipboardContext, String> {
        if self.0.is_none() {
            self.0 = Some(ClipboardContext::new().map_err(error)?);
        }
        Ok(self.0.as_ref().expect("initialized clipboard"))
    }
}
/// The private format is the original 8-byte LE width/height header followed
/// by top-down BGRA8. Standard image exchange deliberately clears alpha.
fn layer_decode(bytes: &[u8], budget: &Budget) -> Result<Pixels, String> {
    if bytes.len() < 8 {
        return Err("invalid Kirikiri clipboard image header".into());
    }
    let size = Size {
        width: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
        height: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
    };
    let len = length(size)?;
    if bytes.len() != len + 8 {
        return Err("invalid Kirikiri clipboard image length".into());
    }
    let mut main = Bytes::zeroed(len, budget).map_err(error)?;
    for (src, dst) in bytes[8..]
        .as_chunks::<4>()
        .0
        .iter()
        .zip(main.as_mut_slice().as_chunks_mut::<4>().0.iter_mut())
    {
        dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
    }
    Ok(Pixels {
        size,
        main: Some(main),
        province: None,
    })
}
fn layer_encode(pixels: &Pixels) -> Result<Vec<u8>, String> {
    let len = length(pixels.size)?;
    let rgba = pixels
        .main
        .as_ref()
        .ok_or("clipboard image has no main plane")?
        .as_slice();
    if rgba.len() != len {
        return Err("clipboard image size mismatch".into());
    }
    let mut bytes = Vec::with_capacity(len + 8);
    bytes.extend(pixels.size.width.to_le_bytes());
    bytes.extend(pixels.size.height.to_le_bytes());
    for src in rgba.as_chunks::<4>().0.iter() {
        bytes.extend([src[2], src[1], src[0], src[3]]);
    }
    Ok(bytes)
}
impl Host for SystemClipboard {
    fn text(&mut self) -> Result<Option<Vec<u16>>, String> {
        let context = self.context()?;
        if !context.has(ContentFormat::Text) {
            return Ok(None);
        }
        let text = context.get_text().map_err(error)?;
        if text.len() > DATA_LIMIT {
            return Err("clipboard text exceeds size limit".into());
        }
        Ok(Some(text.encode_utf16().take_while(|&u| u != 0).collect()))
    }
    fn buffer(&mut self, format: &str) -> Result<Option<Vec<u8>>, String> {
        let context = self.context()?;
        if !context
            .available_formats()
            .map_err(error)?
            .iter()
            .any(|s| s == format)
        {
            return Ok(None);
        }
        let bytes = context.get_buffer(format).map_err(error)?;
        if bytes.len() > DATA_LIMIT + 8 {
            return Err("clipboard data exceeds size limit".into());
        }
        Ok(Some(bytes))
    }
    fn image(&mut self, budget: &Budget) -> Result<Option<Pixels>, String> {
        // Native get_buffer cannot expose size before its allocation; retain
        // the admitted maximum while the private bytes and decode coexist.
        let private_permit = budget.reserve(DATA_LIMIT + 8).map_err(error)?;
        if let Some(bytes) = self.buffer(LAYER_FORMAT)? {
            return layer_decode(&bytes, budget).map(Some);
        }
        drop(private_permit);
        let context = self.context()?;
        if !context.has(ContentFormat::Image) {
            return Ok(None);
        }
        // clipboard-rs owns its native decode; reserve the maximum admitted
        // image before entering it, and charge its RGBA conversion separately.
        let _native = budget.reserve(DATA_LIMIT).map_err(error)?;
        let image = context.get_image().map_err(error)?;
        let (width, height) = image.get_size();
        let size = Size { width, height };
        let len = length(size)?;
        let permit = budget.reserve(len).map_err(error)?;
        let mut rgba = image.to_rgba8().map_err(error)?.into_raw();
        if rgba.len() != len {
            return Err("clipboard image size mismatch".into());
        }
        for pixel in rgba.as_chunks_mut::<4>().0.iter_mut() {
            pixel[3] = 255;
        }
        Ok(Some(Pixels {
            size,
            main: Some(Bytes::with_permit(rgba, permit)),
            province: None,
        }))
    }
    fn write(&mut self, data: Data) -> Result<(), String> {
        let Prepared {
            content,
            formats,
            _permits: permits,
        } = prepare(data)?;
        let context = self.context()?;
        context.set(content).map_err(error)?;
        // Some clipboard-rs backends swallow individual format errors. Report
        // incomplete publication rather than acknowledging a missing format.
        let available = context.available_formats().map_err(error)?;
        for format in formats {
            let present = match format {
                ContentFormat::Other(name) => available.iter().any(|n| n == &name),
                format => context.has(format),
            };
            if !present {
                return Err("clipboard replacement did not publish every requested format".into());
            }
        }
        drop(permits);
        Ok(())
    }
    fn has(&mut self, format: i32) -> Result<bool, String> {
        if !matches!(format, 1..=3) {
            return Ok(false);
        }
        let context = self.context()?;
        Ok(match format {
            1 => context.has(ContentFormat::Text),
            2 => {
                context.has(ContentFormat::Image)
                    || context
                        .available_formats()
                        .map_err(error)?
                        .iter()
                        .any(|s| s == LAYER_FORMAT)
            }
            3 => context
                .available_formats()
                .map_err(error)?
                .iter()
                .any(|s| s == TJS_FORMAT),
            _ => unreachable!(),
        })
    }
    fn watch(&mut self, changed: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Watch>, String> {
        let mut watcher = ClipboardWatcherContext::new().map_err(error)?;
        watcher.add_handler(Handler(changed.clone()));
        let shutdown = watcher.get_shutdown_channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let (thread_stopping, thread_failure) = (stopping.clone(), failure.clone());
        std::thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    watcher.start_watch()
                }));
                if !thread_stopping.load(Ordering::Acquire) {
                    *thread_failure.lock().unwrap() = Some(
                        if result.is_err() {
                            "clipboard watcher failed"
                        } else {
                            "clipboard watcher stopped unexpectedly"
                        }
                        .into(),
                    );
                    changed();
                }
            })
            .map_err(error)?;
        Ok(Box::new(Subscription {
            shutdown: Some(shutdown),
            stopping,
            failure,
        }))
    }
}
/// Pure conversion boundary, separate from all platform clipboard access.
struct Prepared {
    content: Vec<ClipboardContent>,
    formats: Vec<ContentFormat>,
    _permits: Vec<krkr_protocol::budget::Permit>,
}
fn prepare(data: Data) -> Result<Prepared, String> {
    let mut content = Vec::new();
    let mut formats = Vec::new();
    let mut permits = Vec::new();
    if let Some(pixels) = data.image {
        let len = length(pixels.size)?;
        let budget = data
            .image_budget
            .ok_or("clipboard image budget is unavailable")?;
        // Materialize the two published planes, then release the source
        // readback before admitting the codec's temporary input/output.
        permits.push(
            budget
                .reserve(
                    len.checked_mul(2)
                        .and_then(|n| n.checked_add(8))
                        .ok_or("clipboard image size overflow")?,
                )
                .map_err(error)?,
        );
        let private = layer_encode(&pixels)?;
        let mut rgba = pixels.main.as_ref().unwrap().as_slice().to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0.iter_mut() {
            pixel[3] = 255;
        }
        let image = image::RgbaImage::from_raw(pixels.size.width, pixels.size.height, rgba)
            .ok_or("invalid clipboard RGBA image")?;
        // Image MUST come first: clipboard-rs' Windows set_image clears
        // inside set(). Text/private/TJS formats are appended afterwards.
        content.push(ClipboardContent::Image(RustImageData::from_dynamic_image(
            image::DynamicImage::ImageRgba8(image),
        )));
        content.push(ClipboardContent::Other(LAYER_FORMAT.into(), private));
        formats.extend([
            ContentFormat::Image,
            ContentFormat::Other(LAYER_FORMAT.into()),
        ]);
        drop(pixels);
        permits.push(
            budget
                .reserve(
                    len.checked_mul(2)
                        .and_then(|n| n.checked_add(65536))
                        .ok_or("clipboard image size overflow")?,
                )
                .map_err(error)?,
        );
    }
    if let Some(text) = data.text {
        if text.len() > DATA_LIMIT / 2 {
            return Err("clipboard text exceeds size limit".into());
        }
        content.push(ClipboardContent::Text(
            String::from_utf16(&text).map_err(error)?,
        ));
        formats.push(ContentFormat::Text);
    }
    if let Some(tjs) = data.tjs {
        if tjs.len() > DATA_LIMIT {
            return Err("clipboard TJS exceeds size limit".into());
        }
        content.push(ClipboardContent::Other(TJS_FORMAT.into(), tjs));
        formats.push(ContentFormat::Other(TJS_FORMAT.into()));
    }
    if content.is_empty() {
        return Err("clipboard replacement has no supported format".into());
    }
    Ok(Prepared {
        content,
        formats,
        _permits: permits,
    })
}
struct Handler(Arc<dyn Fn() + Send + Sync>);
impl ClipboardHandler for Handler {
    fn on_clipboard_change(&mut self) {
        (self.0)();
    }
}
struct Subscription {
    shutdown: Option<WatcherShutdown>,
    stopping: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}
impl Watch for Subscription {
    fn error(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // This never constructs ClipboardContext or a watcher. It verifies the
    // exact conversion called by SystemClipboard and the PC 2K admission.
    #[test]
    fn clipboard_conversion_preserves_private_alpha_and_admits_2k_in_shared_pool() {
        let budget = Budget::new(64 * 1024 * 1024);
        let size = Size {
            width: 2560,
            height: 1440,
        };
        let len = size.rgba_bytes().unwrap();
        let mut main = Bytes::zeroed(len, &budget).unwrap();
        main.as_mut_slice()[..4].copy_from_slice(&[18, 52, 86, 17]);
        main.as_mut_slice()[len - 4..].copy_from_slice(&[152, 118, 84, 128]);
        let prepared = prepare(Data {
            text: Some("together".encode_utf16().collect()),
            tjs: Some(vec![37, 0, 91, 0, 93, 0, 0, 0]),
            image: Some(Arc::new(Pixels {
                size,
                main: Some(main),
                province: None,
            })),
            image_budget: Some(budget.clone()),
        })
        .unwrap();
        assert!(budget.used() < budget.limit());
        assert_eq!(prepared.content.len(), 4);
        let ClipboardContent::Image(image) = &prepared.content[0] else {
            panic!("image must precede other formats");
        };
        let rgba = image.to_rgba8().unwrap();
        assert_eq!(&rgba.as_raw()[..4], &[18, 52, 86, 255]);
        assert_eq!(&rgba.as_raw()[len - 4..], &[152, 118, 84, 255]);
        drop(rgba);
        let ClipboardContent::Other(format, wire) = &prepared.content[1] else {
            panic!("private image format");
        };
        assert_eq!(format, LAYER_FORMAT);
        assert_eq!(&wire[..8], &[0, 10, 0, 0, 160, 5, 0, 0]);
        assert_eq!(&wire[8..12], &[86, 52, 18, 17]);
        assert_eq!(&wire[wire.len() - 4..], &[84, 118, 152, 128]);
        let Prepared {
            mut content,
            _permits: permits,
            ..
        } = prepared;
        let ClipboardContent::Other(_, wire) = content.remove(1) else {
            unreachable!()
        };
        drop(content);
        drop(permits);
        let wire_permit = budget.reserve(wire.capacity()).unwrap();
        let pixels = layer_decode(&wire, &budget).unwrap();
        assert_eq!(
            &pixels.main.as_ref().unwrap().as_slice()[..4],
            &[18, 52, 86, 17]
        );
        assert_eq!(
            &pixels.main.as_ref().unwrap().as_slice()[len - 4..],
            &[152, 118, 84, 128]
        );
        assert!(layer_decode(&wire[..7], &budget).is_err());
        drop(pixels);
        drop(wire);
        drop(wire_permit);
        assert_eq!(budget.used(), 0);
    }
}
