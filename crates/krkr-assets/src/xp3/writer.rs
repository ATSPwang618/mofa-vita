//! Standard, unencrypted XP3 output, with bounded adaptive segments and a raw index.
use super::{SIGNATURE, portable::portable_name};
use crate::{Error, Limits, Result};
use adler2::Adler32;
use std::{
    collections::BTreeSet,
    io::{Read, Seek, SeekFrom, Write},
};

#[derive(Clone, Copy, Debug, Default)]
pub enum Compression {
    None,
    #[default]
    Zlib,
    /// Fast, independently compressed 256 KiB segments for textures and text.
    /// Media and segments saving less than 12.5% remain directly seekable.
    Auto,
}

/// Start on a new empty stream at offset zero. Any error poisons the writer;
/// callers should discard temporary output unless `finish` succeeds.
pub struct Writer<W> {
    output: W,
    index: Vec<u8>,
    names: BTreeSet<Vec<u16>>,
    limits: Limits,
    compression: Compression,
    failed: bool,
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(mut output: W, compression: Compression, limits: Limits) -> Result<Self> {
        if output.stream_position()? != 0 || output.seek(SeekFrom::End(0))? != 0 {
            return Err(Error::Format("XP3 output must be empty"));
        }
        output.write_all(SIGNATURE)?;
        output.write_all(&0u64.to_le_bytes())?;
        Ok(Self {
            output,
            index: Vec::new(),
            names: BTreeSet::new(),
            limits,
            compression,
            failed: false,
        })
    }

    pub fn add(&mut self, name: &[u16], source: &mut impl Read) -> Result<u64> {
        if self.failed {
            return Err(Error::Format("XP3 writer previously failed"));
        }
        self.failed = true;
        let name = portable_name(name)?;
        if self.names.len() >= self.limits.max_entries {
            return Err(Error::Limit("entry count"));
        }
        if !self.names.insert(name.clone()) {
            return Err(Error::Name("duplicate normalized XP3 entry"));
        }
        let name_len = u16::try_from(name.len()).map_err(|_| Error::Limit("XP3 name"))?;
        // File header + info/segm/adlr headers + fixed fields + UTF-16 name.
        let entry_bytes = 102 + name.len() * 2;
        if entry_bytes > self.limits.max_index_bytes.saturating_sub(self.index.len()) {
            return Err(Error::Limit("index"));
        }
        let offset = self.output.stream_position()?;
        let mut segments = Vec::new();
        let (size, hash) = match self.compression {
            Compression::Auto => self.copy_auto(&name, source, entry_bytes, &mut segments)?,
            Compression::None => copy(source, &mut self.output)?,
            Compression::Zlib => {
                let mut encoder = flate2::write::ZlibEncoder::new(
                    &mut self.output,
                    flate2::Compression::default(),
                );
                let result = copy(source, &mut encoder)?;
                encoder.finish()?;
                result
            }
        };
        let stored = self.output.stream_position()? - offset;
        if segments.is_empty() {
            segments.push((
                matches!(self.compression, Compression::Zlib),
                offset,
                size,
                stored,
            ));
        }
        let entry_bytes = entry_bytes + (segments.len() - 1) * 28;
        if entry_bytes > self.limits.max_index_bytes.saturating_sub(self.index.len()) {
            return Err(Error::Limit("index"));
        }
        let mut body = Vec::with_capacity(entry_bytes - 12);
        header(&mut body, b"info", 22 + u64::from(name_len) * 2)?;
        body.write_all(&0u32.to_le_bytes())?; // No protection or encryption.
        body.write_all(&size.to_le_bytes())?;
        body.write_all(&stored.to_le_bytes())?;
        body.write_all(&name_len.to_le_bytes())?;
        for unit in name {
            body.write_all(&unit.to_le_bytes())?;
        }
        header(&mut body, b"segm", segments.len() as u64 * 28)?;
        for (compressed, offset, size, stored) in segments {
            body.write_all(&u32::from(compressed).to_le_bytes())?;
            for value in [offset, size, stored] {
                body.write_all(&value.to_le_bytes())?;
            }
        }
        header(&mut body, b"adlr", 4)?;
        body.write_all(&hash.to_le_bytes())?;
        header(&mut self.index, b"File", body.len() as u64)?;
        self.index.extend_from_slice(&body);
        self.failed = false;
        Ok(size)
    }

    fn copy_auto(
        &mut self,
        name: &[u16],
        source: &mut impl Read,
        entry_bytes: usize,
        segments: &mut Vec<(bool, u64, u64, u64)>,
    ) -> Result<(u64, u32)> {
        // Identify texture containers by content: resource links may preserve
        // an old image extension. Do not put a seek/decode layer on media.
        let mut prefix = Vec::with_capacity(12);
        source.take(12).read_to_end(&mut prefix)?;
        let text = String::from_utf16_lossy(name).to_ascii_lowercase();
        let extension = text.rsplit('.').next().unwrap_or("");
        let candidate = prefix.starts_with(b"\xabKTX 11\xbb\r\n\x1a\n")
            || prefix.starts_with(b"KBCT\x01\0\0\0")
            || matches!(
                extension,
                "tjs" | "ks" | "txt" | "csv" | "json" | "xml" | "krkr-link"
            );
        let mut input = std::io::Cursor::new(prefix).chain(source);
        if !candidate {
            return copy(&mut input, &mut self.output);
        }
        let mut buffer = vec![0; 256 * 1024];
        let mut hash = Adler32::new();
        let mut size = 0u64;
        loop {
            let mut count = 0;
            while count < buffer.len() {
                match input.read(&mut buffer[count..]) {
                    Ok(0) => break,
                    Ok(n) => count += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            if count == 0 {
                break;
            }
            let data = &buffer[..count];
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(data)?;
            let packed = encoder.finish()?;
            let compressed = packed.len() + 32 <= count - count / 8;
            let payload = if compressed { packed.as_slice() } else { data };
            let offset = self.output.stream_position()?;
            // Check growing metadata before adding the segment, including the
            // fixed file record already accounted for by add().
            if entry_bytes + segments.len() * 28
                > self.limits.max_index_bytes.saturating_sub(self.index.len())
            {
                return Err(Error::Limit("index"));
            }
            self.output.write_all(payload)?;
            segments.push((compressed, offset, count as u64, payload.len() as u64));
            hash.write_slice(data);
            size = size
                .checked_add(count as u64)
                .ok_or(Error::Limit("file size"))?;
        }
        Ok((size, hash.checksum()))
    }

    pub fn finish(mut self) -> Result<W> {
        if self.failed {
            return Err(Error::Format("XP3 writer previously failed"));
        }
        let offset = self.output.stream_position()?;
        self.output.write_all(&[0])?;
        self.output
            .write_all(&(self.index.len() as u64).to_le_bytes())?;
        self.output.write_all(&self.index)?;
        self.output.seek(SeekFrom::Start(11))?;
        self.output.write_all(&offset.to_le_bytes())?;
        self.output.seek(SeekFrom::End(0))?;
        self.output.flush()?;
        Ok(self.output)
    }
}

fn header(out: &mut impl Write, tag: &[u8; 4], size: u64) -> Result<()> {
    out.write_all(tag)?;
    out.write_all(&size.to_le_bytes())?;
    Ok(())
}

pub(super) fn copy(source: &mut impl Read, output: &mut impl Write) -> Result<(u64, u32)> {
    let mut buffer = [0; 64 * 1024];
    let mut hash = Adler32::new();
    let mut size = 0u64;
    loop {
        let count = match source.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hash.write_slice(&buffer[..count]);
        size = size
            .checked_add(count as u64)
            .ok_or(Error::Limit("file size"))?;
    }
    Ok((size, hash.checksum()))
}
