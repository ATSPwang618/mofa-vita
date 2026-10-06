//! Visible dimensions and timing of the normalized, non-fragmented AVC MP4
//! profile. AvPlayer frame dimensions describe macroblock storage instead.
use krkr_protocol::graphics::Size;
use std::io::{Read, Seek, SeekFrom};

pub struct Format {
    pub size: Size,
    pub fps: f64,
    pub frames: u64,
}
#[derive(Clone, Copy)]
struct Atom {
    kind: [u8; 4],
    start: u64,
    end: u64,
}
struct Reader<'a, R: ?Sized> {
    stream: &'a mut R,
    boxes_left: usize,
}
type Result<T> = std::result::Result<T, String>;
impl<R: Read + Seek + ?Sized> Reader<'_, R> {
    fn bytes<const N: usize>(&mut self, atom: Atom, offset: u64) -> Result<[u8; N]> {
        let start = atom
            .start
            .checked_add(offset)
            .ok_or("MP4 offset overflow")?;
        if start.checked_add(N as u64).is_none_or(|end| end > atom.end) {
            return Err("truncated MP4 metadata".into());
        }
        // Preserve the input read-ahead when nearby MP4 tables share a block.
        // Absolute BufReader seeks discard it and reopen XP3 segments on each
        // nonsequential metadata read.
        let current = self.stream.stream_position().map_err(|e| e.to_string())?;
        if let Ok(delta) = i64::try_from(i128::from(start) - i128::from(current)) {
            self.stream
                .seek_relative(delta)
                .map_err(|e| e.to_string())?;
        } else {
            self.stream
                .seek(SeekFrom::Start(start))
                .map_err(|e| e.to_string())?;
        }
        let mut bytes = [0; N];
        self.stream
            .read_exact(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
    fn children(&mut self, parent: Atom, skip: u64) -> Result<Vec<Atom>> {
        let mut at = parent
            .start
            .checked_add(skip)
            .filter(|&n| n <= parent.end)
            .ok_or("truncated MP4 container")?;
        let mut out = Vec::new();
        while at < parent.end {
            self.boxes_left = self.boxes_left.checked_sub(1).ok_or("too many MP4 boxes")?;
            let raw = Atom {
                kind: [0; 4],
                start: at,
                end: parent.end,
            };
            let header = self.bytes::<8>(raw, 0)?;
            let size = u32::from_be_bytes(header[..4].try_into().unwrap());
            let (size, header_size) = match size {
                0 => (parent.end - at, 8),
                1 => (u64::from_be_bytes(self.bytes(raw, 8)?), 16),
                n => (u64::from(n), 8),
            };
            let end = at
                .checked_add(size)
                .filter(|&n| n <= parent.end && size >= header_size)
                .ok_or("invalid MP4 box size")?;
            out.push(Atom {
                kind: header[4..].try_into().unwrap(),
                start: at + header_size,
                end,
            });
            at = end;
        }
        Ok(out)
    }
}
fn find(atoms: &[Atom], kind: &[u8; 4]) -> Result<Atom> {
    atoms
        .iter()
        .find(|a| &a.kind == kind)
        .copied()
        .ok_or_else(|| format!("missing MP4 {} box", String::from_utf8_lossy(kind)))
}

pub fn read(stream: &mut (impl Read + Seek + ?Sized), length: u64) -> Result<Format> {
    let mut r = Reader {
        stream,
        boxes_left: 4096,
    };
    let root = Atom {
        kind: [0; 4],
        start: 0,
        end: length,
    };
    let top = r.children(root, 0)?;
    let movie = r.children(find(&top, b"moov")?, 0)?;
    for track in movie.into_iter().filter(|a| &a.kind == b"trak") {
        let track = r.children(track, 0)?;
        let media = r.children(find(&track, b"mdia")?, 0)?;
        if &r.bytes::<4>(find(&media, b"hdlr")?, 8)? != b"vide" {
            continue;
        }
        let header = find(&media, b"mdhd")?;
        let timescale_at = match r.bytes::<1>(header, 0)?[0] {
            0 => 12,
            1 => 20,
            _ => return Err("unsupported MP4 media header version".into()),
        };
        let timescale = u32::from_be_bytes(r.bytes(header, timescale_at)?);
        let minf = r.children(find(&media, b"minf")?, 0)?;
        let samples = r.children(find(&minf, b"stbl")?, 0)?;
        let descriptions = find(&samples, b"stsd")?;
        if u32::from_be_bytes(r.bytes(descriptions, 4)?) != 1 {
            return Err("PSV movie requires one AVC sample description".into());
        }
        let entries = r.children(descriptions, 8)?;
        if entries.len() != 1 || !matches!(&entries[0].kind, b"avc1" | b"avc3") {
            return Err("PSV movie requires AVC video".into());
        }
        let dimensions = r.bytes::<4>(entries[0], 24)?;
        let size = Size {
            width: u16::from_be_bytes(dimensions[..2].try_into().unwrap()).into(),
            height: u16::from_be_bytes(dimensions[2..].try_into().unwrap()).into(),
        };
        if size.width == 0
            || size.height == 0
            || size.width > 960
            || size.height > 544
            || !size.width.is_multiple_of(2)
            || !size.height.is_multiple_of(2)
        {
            return Err("PSV movie requires even visible dimensions up to 960x544".into());
        }
        let timing = find(&samples, b"stts")?;
        let count = u32::from_be_bytes(r.bytes(timing, 4)?);
        if count > 65536 {
            return Err("MP4 timing table exceeds limit".into());
        }
        let (mut frames, mut ticks) = (0u64, 0u64);
        for i in 0..count {
            let entry = r.bytes::<8>(timing, 8 + u64::from(i) * 8)?;
            let n = u64::from(u32::from_be_bytes(entry[..4].try_into().unwrap()));
            let delta = u64::from(u32::from_be_bytes(entry[4..].try_into().unwrap()));
            if n == 0 || delta == 0 {
                return Err("invalid MP4 frame timing".into());
            }
            frames = frames.checked_add(n).ok_or("MP4 sample count overflow")?;
            ticks = ticks
                .checked_add(n * delta)
                .ok_or("MP4 duration overflow")?;
        }
        if timescale == 0 || ticks == 0 {
            return Err("missing MP4 frame timing".into());
        }
        return Ok(Format {
            size,
            fps: timescale as f64 * frames as f64 / ticks as f64,
            frames,
        });
    }
    Err("MP4 has no video track".into())
}

/// Compact the visible rectangle from a padded NV12 surface. In particular UV
/// starts after *all* padded Y rows, not immediately after the visible rows.
/// Visit every visible destination element exactly once, after validating all
/// dimensions. Generic destination elements allow native callers to initialize
/// spare byte capacity directly without a redundant zero-fill.
pub fn copy_nv12<T>(
    source: &[u8],
    storage: Size,
    target: &mut [T],
    visible: Size,
    mut copy: impl FnMut(&mut [T], &[u8]),
) -> Result<()> {
    use krkr_protocol::pixels::Yuv420;
    if visible.width == 0
        || visible.height == 0
        || storage.width > 960
        || storage.height > 544
        || visible.width > storage.width
        || visible.height > storage.height
        || storage.width - visible.width >= 16
        || storage.height - visible.height >= 16
        || Yuv420::byte_len(storage) != Some(source.len())
        || Yuv420::byte_len(visible) != Some(target.len())
    {
        return Err("AvPlayer buffer does not match visible MP4 dimensions".into());
    }
    if storage == visible {
        // Fully macroblock-aligned movies need a single synchronous DMA,
        // including the adjacent Y and UV planes.
        copy(target, source);
        return Ok(());
    }
    let stride = storage.width as usize;
    let width = visible.width as usize;
    let height = visible.height as usize;
    let uv = stride * storage.height as usize;
    if stride == width {
        // Normalized 960x540 movies have only bottom padding. Copy entire
        // planes, not 810 separate rows, so the native host can use DMA.
        let y = width * height;
        copy(&mut target[..y], &source[..y]);
        copy(&mut target[y..], &source[uv..uv + y / 2]);
        return Ok(());
    }
    for row in 0..height {
        copy(
            &mut target[row * width..(row + 1) * width],
            &source[row * stride..row * stride + width],
        );
    }
    for row in 0..height / 2 {
        copy(
            &mut target[(height + row) * width..(height + row + 1) * width],
            &source[uv + row * stride..uv + row * stride + width],
        );
    }
    Ok(())
}
