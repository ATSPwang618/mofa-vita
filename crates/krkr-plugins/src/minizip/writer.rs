use super::{archive::cancel, error};
use krkr_engine::assets::{Limits, ReadPlan};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};
use tjs_core::NativeResult;
use zip::{
    CompressionMethod, ZipWriter, unstable::write::FileOptionsExt, write::SimpleFileOptions,
};

struct Output {
    file: File,
    limit: u64,
}
impl Read for Output {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.file.read(out)
    }
}
impl Seek for Output {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.file.seek(from)
    }
}
impl Write for Output {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() as u64 > self.limit.saturating_sub(self.file.stream_position()?) {
            return Err(io::Error::other("ZIP exceeds storage write budget"));
        }
        self.file.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
pub(super) struct Writer {
    zip: Option<ZipWriter<Output>>,
    flags: BTreeMap<String, u16>,
    limits: Limits,
    index_bytes: usize,
    entries: usize,
}
impl Writer {
    pub fn open(path: &Path, mode: i32, limits: Limits) -> NativeResult<Self> {
        let append = mode == 2 && path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(mode != 0)
            .create_new(mode == 0)
            .truncate(!append && mode != 0)
            .open(path)
            .map_err(error)?;
        let mut output = Output {
            file,
            limit: limits.max_read_bytes as u64,
        };
        let (zip, entries, index_bytes, flags) = if append {
            super::archive::admit(&mut output, limits).map_err(error)?;
            let mut old = zip::ZipArchive::new(output).map_err(error)?;
            let mut bytes = 0usize;
            let mut offsets = Vec::new();
            if old.len() > limits.max_entries {
                return Err(error("ZIP entries exceed budget"));
            }
            for i in 0..old.len() {
                let entry = old.by_index_raw(i).map_err(error)?;
                bytes = bytes.saturating_add(entry.name_raw().len() * 4 + 512);
                offsets.push((entry.name().to_owned(), entry.central_header_start() + 8));
            }
            if bytes > limits.max_index_bytes {
                return Err(error("ZIP index exceeds budget"));
            }
            let entries = old.len();
            let mut output = old.into_inner();
            let mut flags = BTreeMap::new();
            for (name, offset) in offsets {
                output.seek(SeekFrom::Start(offset)).map_err(error)?;
                let mut raw = [0; 2];
                output.read_exact(&mut raw).map_err(error)?;
                flags.insert(name, u16::from_le_bytes(raw) & 0x806);
            }
            (
                ZipWriter::new_append(output).map_err(error)?,
                entries,
                bytes,
                flags,
            )
        } else {
            (ZipWriter::new(output), 0, 0, BTreeMap::new())
        };
        Ok(Self {
            zip: Some(zip),
            flags,
            limits,
            index_bytes,
            entries,
        })
    }
    pub fn add(
        &mut self,
        plan: ReadPlan,
        name: String,
        level: i32,
        password: Option<Vec<u8>>,
        check: &dyn Fn() -> bool,
    ) -> NativeResult<bool> {
        cancel(check).map_err(error)?;
        if !(-1..=9).contains(&level) {
            return Ok(false);
        }
        let bytes = name.len().saturating_mul(4).saturating_add(512);
        if self.entries >= self.limits.max_entries
            || bytes > self.limits.max_index_bytes.saturating_sub(self.index_bytes)
            || plan.bytes > self.limits.max_read_bytes as u64
        {
            return Err(error("ZIP entry exceeds storage budget"));
        }
        let timestamp = krkr_engine::assets::local::from_storage(&plan.name)
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| jiff::Timestamp::try_from(t).ok())
            .unwrap_or_else(jiff::Timestamp::now);
        let date = tjs_bind::date::timezone::system().to_datetime(timestamp)?;
        let date = zip::DateTime::from_date_and_time(
            date.year() as u16,
            date.month() as u8,
            date.day() as u8,
            date.hour() as u8,
            date.minute() as u8,
            date.second() as u8,
        )
        .unwrap_or_default();
        let mut options = SimpleFileOptions::default()
            .last_modified_time(date)
            .large_file(plan.bytes >= u64::from(u32::MAX))
            .compression_method(if level == 0 {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            })
            .compression_level((level > 0).then_some(i64::from(level)));
        // Original NarrowString returns a null pointer for an empty password.
        if let Some(p) = password.as_deref().filter(|p| !p.is_empty()) {
            options = options.with_deprecated_encryption(p).map_err(error)?;
        }
        let mut input = plan.open_interruptible(check).map_err(error)?;
        let zip = self.zip.as_mut().expect("open ZIP writer");
        if zip.start_file(&name, options).is_err() {
            return Ok(false);
        }
        let result = (|| -> NativeResult<()> {
            let mut left = plan.bytes;
            let mut buffer = [0; 16384];
            while left > 0 {
                cancel(check).map_err(error)?;
                let n = left.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..n]).map_err(error)?;
                zip.write_all(&buffer[..n]).map_err(error)?;
                left -= n as u64;
            }
            // Trigger decoder CRC validation even when the expected size is exact.
            let mut tail = [0];
            if input.read(&mut tail).map_err(error)? != 0 {
                return Err(error("ZIP source size changed"));
            }
            cancel(check).map_err(error)?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = zip.abort_file();
            return Err(e);
        }
        let compression_flags = match level {
            8 | 9 => 2,
            2 => 4,
            1 => 6,
            _ => 0,
        };
        self.flags.insert(name, 0x800 | compression_flags);
        self.index_bytes += bytes;
        self.entries += 1;
        Ok(true)
    }
    pub fn close(&mut self) -> NativeResult<()> {
        let Some(zip) = self.zip.take() else {
            return Ok(());
        };
        let output = zip.finish().map_err(error)?;
        let mut archive = zip::ZipArchive::new(output).map_err(error)?;
        let mut edits = Vec::new();
        for i in 0..archive.len() {
            let entry = archive.by_index_raw(i).map_err(error)?;
            if let Some(&flags) = self.flags.get(entry.name()) {
                edits.push((
                    entry.header_start() + 6,
                    entry.central_header_start() + 8,
                    flags,
                ));
            }
        }
        let mut output = archive.into_inner();
        for (local, central, flags) in edits {
            for offset in [local, central] {
                output.seek(SeekFrom::Start(offset)).map_err(error)?;
                let mut old = [0; 2];
                output.read_exact(&mut old).map_err(error)?;
                let flags = (u16::from_le_bytes(old) & !0x806) | flags;
                output.seek(SeekFrom::Start(offset)).map_err(error)?;
                output.write_all(&flags.to_le_bytes()).map_err(error)?;
            }
        }
        output.flush().map_err(error)
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
