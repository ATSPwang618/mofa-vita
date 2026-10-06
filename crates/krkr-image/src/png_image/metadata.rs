//! PNG text is metadata, never pixel geometry. Native coordinate chunks win
//! over text with the same key; otherwise the last occurrence wins, as in TLG.
use super::*;
use std::{borrow::Cow, io::Read};

const LIMIT: usize = 64 * 1024;
const COUNT: usize = 1024;

#[derive(Default)]
struct Retained {
    bytes: usize,
    count: usize,
}
impl Retained {
    fn add(&mut self, key: &str, value: &str) -> Result<()> {
        self.bytes = self
            .bytes
            .saturating_add(key.len())
            .saturating_add(value.len());
        self.count += 1;
        if self.bytes > LIMIT || self.count > COUNT {
            return Err(Error::Message("PNG metadata budget exceeded"));
        }
        Ok(())
    }
}
fn field<'a>(data: &mut &'a [u8]) -> Result<&'a [u8]> {
    let at = data
        .iter()
        .position(|&b| b == 0)
        .ok_or(Error::Message("invalid PNG text field"))?;
    let value = &data[..at];
    *data = &data[at + 1..];
    Ok(value)
}
fn keyword(data: &[u8]) -> Result<()> {
    if data.is_empty()
        || data.len() > 79
        || data.first() == Some(&b' ')
        || data.last() == Some(&b' ')
        || data.windows(2).any(|p| p == b"  ")
        || data.iter().any(|b| !matches!(b, 32..=126 | 161..=255))
    {
        return Err(Error::Message("invalid PNG text keyword"));
    }
    Ok(())
}
fn latin(data: &[u8]) -> String {
    data.iter().map(|&b| char::from(b)).collect()
}
fn inflated(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    decoder
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(error)?;
    if out.len() > limit {
        return Err(Error::Message("PNG metadata budget exceeded"));
    }
    Ok(out)
}
fn text(kind: &[u8], mut data: &[u8], retained: &mut Retained) -> Result<(String, String)> {
    let name = field(&mut data)?;
    keyword(name)?;
    let name = latin(name);
    let limit = LIMIT
        .saturating_sub(retained.bytes)
        .saturating_sub(name.len());
    let mut utf8 = false;
    let compressed = match kind {
        b"tEXt" => false,
        b"zTXt" => {
            if data.first() != Some(&0) {
                return Err(Error::Message("invalid PNG text compression"));
            }
            data = &data[1..];
            true
        }
        b"iTXt" => {
            if data.len() < 2 || data[0] > 1 || (data[0] == 1 && data[1] != 0) {
                return Err(Error::Message("invalid PNG text compression"));
            }
            let compressed = data[0] == 1;
            data = &data[2..];
            let language = field(&mut data)?;
            let translated = field(&mut data)?;
            if !language.is_ascii() || std::str::from_utf8(translated).is_err() {
                return Err(Error::Message("invalid PNG international text header"));
            }
            utf8 = true;
            compressed
        }
        _ => unreachable!(),
    };
    let data = if compressed {
        Cow::Owned(inflated(data, limit)?)
    } else {
        if data.len() > limit {
            return Err(Error::Message("PNG metadata budget exceeded"));
        }
        Cow::Borrowed(data)
    };
    if data.contains(&0) {
        return Err(Error::Message("null byte in PNG text"));
    }
    let value = if utf8 {
        std::str::from_utf8(&data).map_err(error)?.to_owned()
    } else {
        latin(&data)
    };
    retained.add(&name, &value)?;
    Ok((name, value))
}
pub(super) fn tags(data: &[u8]) -> Result<Tags> {
    let mut text_tags = Vec::new();
    let mut native = Vec::new();
    let mut retained = Retained::default();
    let mut offset = 8usize;
    while offset + 12 <= data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let tag = &data[offset + 4..offset + 8];
        let end = offset
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .filter(|&n| n <= data.len())
            .ok_or(Error::Message("truncated PNG chunk"))?;
        let payload = &data[offset + 8..end - 4];
        if matches!(tag, b"tEXt" | b"zTXt" | b"iTXt") {
            text_tags.push(text(tag, payload, &mut retained)?);
        } else if length >= 9 {
            let names = if tag == b"oFFs" {
                Some((
                    ["offs_x", "offs_y", "offs_unit"],
                    true,
                    match payload[8] {
                        0 => "pixel",
                        1 => "micrometer",
                        _ => "unknown",
                    },
                ))
            } else if tag == b"pHYs" {
                Some((
                    ["reso_x", "reso_y", "reso_unit"],
                    false,
                    if payload[8] == 1 { "meter" } else { "unknown" },
                ))
            } else if tag.eq_ignore_ascii_case(b"vpAg") {
                Some((
                    ["vpag_w", "vpag_h", "vpag_unit"],
                    true,
                    match payload[8] {
                        0 => "pixel",
                        1 => "micrometer",
                        _ => "unknown",
                    },
                ))
            } else {
                None
            };
            if let Some((names, signed, unit)) = names {
                let x = u32::from_be_bytes(payload[..4].try_into().unwrap());
                let y = u32::from_be_bytes(payload[4..8].try_into().unwrap());
                for (name, value) in names.into_iter().zip([
                    if signed {
                        (x as i32).to_string()
                    } else {
                        x.to_string()
                    },
                    if signed {
                        (y as i32).to_string()
                    } else {
                        y.to_string()
                    },
                    unit.to_owned(),
                ]) {
                    retained.add(name, &value)?;
                    native.push((name.into(), value));
                }
            }
        }
        offset = end;
        if tag == b"IEND" {
            break;
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    text_tags.extend(native);
    text_tags.reverse();
    text_tags.retain(|(name, _)| seen.insert(name.clone()));
    text_tags.reverse();
    Ok(text_tags)
}

/// Emit ordinary uncompressed iTXt: Latin-1 keywords, UTF-8 values. No private
/// chunk or geometry mutation is needed to retain game atlas and mode tags.
pub(crate) fn write_tags<W: std::io::Write>(
    writer: &mut png::Writer<W>,
    tags: &Tags,
) -> Result<()> {
    let mut retained = Retained::default();
    for (name, value) in tags {
        retained.add(name, value)?;
        let key: Vec<u8> = name
            .chars()
            .map(|c| u8::try_from(u32::from(c)))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| Error::Message("PNG text keyword must be Latin-1"))?;
        keyword(&key)?;
        if value.contains('\0') {
            return Err(Error::Message("null byte in PNG text"));
        }
        let mut data = Vec::with_capacity(key.len() + value.len() + 5);
        data.extend(key);
        data.extend([0; 5]); // separator, compression flag/method, language, translated keyword
        data.extend(value.as_bytes());
        writer.write_chunk(png::chunk::iTXt, &data).map_err(error)?;
    }
    Ok(())
}
