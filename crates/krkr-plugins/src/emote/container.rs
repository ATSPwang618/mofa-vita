//! File-level wrappers are decoded before the script's mutable PSB callback.
use std::io::Read;
type Result<T> = std::result::Result<T, &'static str>;
const LIMIT: usize = 64 * 1024 * 1024;
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(n)
            .ok_or("E-mote stream overflow")?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or("truncated E-mote stream")?;
        self.position = end;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn word(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn length(&mut self, base: u8) -> Result<usize> {
        let mut length = usize::from(base);
        if base == 15 {
            loop {
                let n = self.byte()?;
                length = length
                    .checked_add(n.into())
                    .filter(|&n| n <= LIMIT)
                    .ok_or("LZ4 length limit exceeded")?;
                if n != 255 {
                    break;
                }
            }
        }
        Ok(length)
    }
}
fn lz4(bytes: &[u8], stop: &dyn Fn() -> bool) -> Result<Vec<u8>> {
    let mut r = Reader { bytes, position: 4 };
    let flags = r.byte()?;
    let block = r.byte()?;
    if flags >> 6 != 1 {
        return Err("unsupported LZ4 frame version");
    }
    let block_limit = match (block >> 4) & 7 {
        4 => 65536,
        5 => 262144,
        6 => 1048576,
        7 => 4194304,
        _ => return Err("invalid LZ4 block size"),
    };
    let size = if flags & 8 != 0 {
        let low = r.word()? as u64;
        let high = r.word()? as u64;
        let n = low | (high << 32);
        if n > LIMIT as u64 {
            return Err("LZ4 size limit exceeded");
        }
        Some(n as usize)
    } else {
        None
    };
    if flags & 1 != 0 {
        r.word()?;
    }
    r.byte()?; // The reference does not verify descriptor/block/content checksums.
    let mut out = Vec::new();
    loop {
        if stop() {
            return Err("E-mote decompression cancelled");
        }
        let header = r.word()?;
        if header == 0 {
            break;
        }
        let size = (header & 0x7fffffff) as usize;
        if size > block_limit {
            return Err("oversized LZ4 block");
        }
        let input = r.take(size)?;
        let start = out.len();
        let maximum = start.checked_add(block_limit).unwrap_or(LIMIT).min(LIMIT);
        if header & 0x80000000 != 0 {
            if start + size > LIMIT {
                return Err("LZ4 size limit exceeded");
            }
            out.extend_from_slice(input);
        } else {
            let mut block = Reader {
                bytes: input,
                position: 0,
            };
            while block.position < input.len() {
                if stop() {
                    return Err("E-mote decompression cancelled");
                }
                let token = block.byte()?;
                let count = block.length(token >> 4)?;
                if out.len().checked_add(count).is_none_or(|n| n > maximum) {
                    return Err("LZ4 literal limit exceeded");
                }
                out.extend_from_slice(block.take(count)?);
                if block.position == input.len() {
                    break;
                }
                let offset = block.take(2)?;
                let distance = u16::from_le_bytes([offset[0], offset[1]]) as usize;
                let available = if flags & 0x20 != 0 {
                    out.len() - start
                } else {
                    out.len()
                };
                if distance == 0 || distance > available {
                    return Err("invalid LZ4 match offset");
                }
                let count = block
                    .length(token & 15)?
                    .checked_add(4)
                    .ok_or("LZ4 match overflow")?;
                if out.len().checked_add(count).is_none_or(|n| n > maximum) {
                    return Err("LZ4 match limit exceeded");
                }
                for i in 0..count {
                    if i % 65536 == 0 && stop() {
                        return Err("E-mote decompression cancelled");
                    }
                    out.push(out[out.len() - distance]);
                }
            }
        }
        if flags & 0x10 != 0 {
            r.word()?;
        }
    }
    if flags & 4 != 0 {
        r.word()?;
    }
    if size.is_some_and(|n| n != out.len()) {
        return Err("LZ4 content size mismatch");
    }
    Ok(out)
}
pub(super) fn unpack(mut bytes: Vec<u8>, seed: i32, stop: &dyn Fn() -> bool) -> Result<Vec<u8>> {
    if bytes.len() > LIMIT {
        return Err("E-mote input limit exceeded");
    }
    if bytes.starts_with(&[4, 0x22, 0x4d, 0x18]) {
        bytes = lz4(&bytes, stop)?;
    } else if bytes
        .get(..3)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"mdf"))
    {
        let mut r = Reader {
            bytes: &bytes,
            position: 4,
        };
        let size = r.word()? as usize;
        if size > LIMIT {
            return Err("MDF size limit exceeded");
        }
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[8..]);
        let mut out = Vec::new();
        let mut block = [0; 16384];
        loop {
            if stop() {
                return Err("E-mote decompression cancelled");
            }
            let n = decoder.read(&mut block).map_err(|_| "invalid MDF stream")?;
            if n == 0 {
                break;
            }
            if out.len() + n > size {
                return Err("MDF size mismatch");
            }
            out.extend_from_slice(&block[..n]);
        }
        bytes = out;
    }
    if seed > 0 {
        let mut r = Reader {
            bytes: &bytes,
            position: 4,
        };
        let version = r.take(2)?;
        let version = u16::from_le_bytes([version[0], version[1]]);
        r.take(2)?;
        let start = r.word()? as usize;
        r.take(12)?;
        let end = r.word()? as usize;
        if version == 2 {
            let body = bytes
                .get_mut(start..end)
                .ok_or("invalid encrypted PSB range")?;
            let mut key = [0x075bcd15u32, 0x159a55e5, 0x1f123bb5, seed as u32];
            let mut word = 0u32;
            for chunk in body.chunks_mut(65536) {
                if stop() {
                    return Err("E-mote decryption cancelled");
                }
                for byte in chunk {
                    if word == 0 {
                        let b = key[3];
                        let a = key[0] ^ (key[0] << 11);
                        let c = a ^ b ^ ((a ^ (b >> 11)) >> 8);
                        key = [key[1], key[2], b, c];
                        word = c;
                    }
                    *byte ^= word as u8;
                    word >>= 8;
                }
            }
        }
    }
    Ok(bytes)
}
