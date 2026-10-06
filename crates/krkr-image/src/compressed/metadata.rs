use crate::{Error, Result, Tags};

pub(super) const TAGS: &[u8] = b"krkr.tags";
pub(super) const CANVAS: &[u8] = b"krkr.canvas";
pub(crate) const LIMIT: usize = 64 * 1024;

pub(crate) fn encode(tags: &Tags) -> Result<Vec<u8>> {
    let size = tags
        .iter()
        .try_fold(8usize, |size, (key, value)| {
            size.checked_add(8)?
                .checked_add(key.len())?
                .checked_add(value.len())
        })
        .ok_or(Error::Message("texture tags size overflow"))?;
    if tags.len() > 1024 || size > LIMIT - 128 {
        return Err(Error::Message("texture tags exceed metadata capacity"));
    }
    let mut out = Vec::with_capacity(size);
    out.extend(1u32.to_le_bytes());
    out.extend((tags.len() as u32).to_le_bytes());
    for (key, value) in tags {
        for text in [key, value] {
            out.extend((text.len() as u32).to_le_bytes());
            out.extend(text.as_bytes());
        }
    }
    Ok(out)
}

pub(crate) fn decode(mut data: &[u8]) -> Result<Tags> {
    fn word(data: &mut &[u8]) -> Result<u32> {
        let bytes = data
            .get(..4)
            .ok_or(Error::Message("truncated texture tags"))?;
        let value = u32::from_le_bytes(bytes.try_into().unwrap());
        *data = &data[4..];
        Ok(value)
    }
    if data.len() > LIMIT || word(&mut data)? != 1 {
        return Err(Error::Message("invalid texture tags version or size"));
    }
    let count = word(&mut data)? as usize;
    if count > 1024 || count > data.len() / 8 {
        return Err(Error::Message("invalid texture tags count"));
    }
    let mut tags = Vec::with_capacity(count);
    for _ in 0..count {
        let mut text = || -> Result<String> {
            let length = word(&mut data)? as usize;
            let bytes = data
                .get(..length)
                .ok_or(Error::Message("truncated texture tag string"))?;
            let value = std::str::from_utf8(bytes)
                .map_err(|_| Error::Message("invalid UTF-8 texture tag"))?
                .to_owned();
            data = &data[length..];
            Ok(value)
        };
        tags.push((text()?, text()?));
    }
    if !data.is_empty() {
        return Err(Error::Message("trailing texture tags data"));
    }
    Ok(tags)
}

pub(super) fn entry(out: &mut Vec<u8>, key: &[u8], value: &[u8]) {
    let length = key.len() + 1 + value.len();
    out.extend((length as u32).to_le_bytes());
    out.extend(key);
    out.push(0);
    out.extend(value);
    out.resize(out.len().next_multiple_of(4), 0);
}
