use super::{Error, Result};
use std::io::{Read, Seek, SeekFrom};

pub(super) struct Reader<R> {
    pub input: R,
    pub pos: u64,
    pub end: u64,
}
impl<R: Read + Seek> Reader<R> {
    pub fn new(input: R, end: u64) -> Self {
        Self { input, pos: 0, end }
    }
    pub fn remaining(&self) -> u64 {
        self.end - self.pos
    }
    pub fn seek(&mut self, pos: u64) -> Result<()> {
        if pos > self.end {
            return Err(Error::Message("truncated PSD section"));
        }
        self.input.seek(SeekFrom::Start(pos))?;
        self.pos = pos;
        Ok(())
    }
    pub fn skip(&mut self, bytes: u64) -> Result<()> {
        let pos = self
            .pos
            .checked_add(bytes)
            .ok_or(Error::Message("PSD offset overflow"))?;
        self.seek(pos)
    }
    pub fn read(&mut self, out: &mut [u8]) -> Result<()> {
        if out.len() as u64 > self.remaining() {
            return Err(Error::Message("truncated PSD section"));
        }
        self.input.read_exact(out)?;
        self.pos += out.len() as u64;
        Ok(())
    }
    pub fn bytes(&mut self, size: usize) -> Result<Vec<u8>> {
        if size > 32 * 1024 * 1024 || size as u64 > self.remaining() {
            return Err(Error::Message("PSD metadata exceeds section limit"));
        }
        let mut out = vec![0; size];
        self.read(&mut out)?;
        Ok(out)
    }
    pub fn u8(&mut self) -> Result<u8> {
        let mut a = [0];
        self.read(&mut a)?;
        Ok(a[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        let mut a = [0; 2];
        self.read(&mut a)?;
        Ok(u16::from_be_bytes(a))
    }
    pub fn u32(&mut self) -> Result<u32> {
        let mut a = [0; 4];
        self.read(&mut a)?;
        Ok(u32::from_be_bytes(a))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn f64(&mut self) -> Result<f64> {
        let mut a = [0; 8];
        self.read(&mut a)?;
        Ok(f64::from_be_bytes(a))
    }
    pub fn key(&mut self) -> Result<[u8; 4]> {
        let mut a = [0; 4];
        self.read(&mut a)?;
        Ok(a)
    }
    pub fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > 100_000 {
            return Err(Error::Message("PSD metadata entry limit exceeded"));
        }
        Ok(n)
    }
    pub fn unicode(&mut self) -> Result<Vec<u16>> {
        let n = self.u32()? as usize;
        let bytes = n
            .checked_mul(2)
            .ok_or(Error::Message("PSD string overflow"))?;
        let data = self.bytes(bytes)?;
        Ok(data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes([p[0], p[1]]))
            .take_while(|&c| c != 0)
            .collect())
    }
    pub fn id(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        let bytes = self.bytes(if n == 0 { 4 } else { n })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
    pub fn block_end(&mut self) -> Result<u64> {
        let n = u64::from(self.u32()?);
        let end = self.pos + n;
        if end > self.end {
            return Err(Error::Message("PSD block exceeds enclosing section"));
        }
        Ok(end)
    }
}
pub(super) fn slice(data: &[u8]) -> Reader<std::io::Cursor<&[u8]>> {
    Reader::new(std::io::Cursor::new(data), data.len() as u64)
}
