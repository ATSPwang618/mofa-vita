use crate::{Error, Result};
pub(crate) struct Cursor<'a>(pub &'a [u8]);
impl<'a> Cursor<'a> {
    pub fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        let (head, tail) = self
            .0
            .split_at_checked(size)
            .ok_or(Error::Format("truncated binary field"))?;
        self.0 = tail;
        Ok(head)
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn next_chunk(&mut self) -> Result<Option<([u8; 4], &'a [u8])>> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let tag = self.take(4)?.try_into().unwrap();
        let len = usize::try_from(self.u64()?).map_err(|_| Error::Limit("index"))?;
        Ok(Some((tag, self.take(len)?)))
    }
}
pub(crate) fn size(size: u64, limit: usize, what: &'static str) -> Result<usize> {
    let n = usize::try_from(size).map_err(|_| Error::Limit(what))?;
    if n > limit {
        return Err(Error::Limit(what));
    }
    Ok(n)
}
pub(crate) fn inflate(bytes: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = vec![
        0;
        expected
            .checked_add(1)
            .ok_or(Error::Limit("decompression"))?
    ];
    let mut decoder = flate2::Decompress::new(true);
    let status = decoder
        .decompress(bytes, &mut out, flate2::FlushDecompress::Finish)
        .map_err(|_| Error::Format("invalid zlib stream"))?;
    if status != flate2::Status::StreamEnd
        || decoder.total_out() != expected as u64
        || decoder.total_in() != bytes.len() as u64
    {
        return Err(Error::Format("zlib length mismatch"));
    }
    out.truncate(expected);
    Ok(out)
}
