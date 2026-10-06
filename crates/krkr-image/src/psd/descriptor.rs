use super::{Error, Result, reader::Reader};
use std::{
    collections::BTreeMap,
    io::{Read, Seek},
};

#[derive(Clone, Debug, Default)]
pub enum Meta {
    #[default]
    Void,
    Int(i64),
    Real(f64),
    Text(Vec<u16>),
    List(Vec<Meta>),
    Map(BTreeMap<String, Meta>),
}
impl Meta {
    pub fn get(&self, key: &str) -> &Self {
        match self {
            Self::Map(m) => m.get(key).unwrap_or(&Self::Void),
            _ => &Self::Void,
        }
    }
    pub fn integer(&self, default: i64) -> i64 {
        match self {
            Self::Int(n) => *n,
            Self::Real(n) => *n as i64,
            _ => default,
        }
    }
    pub fn list(&self) -> &[Self] {
        match self {
            Self::List(v) => v,
            _ => &[],
        }
    }
    pub fn text(&self) -> Self {
        match self {
            Self::Text(_) => self.clone(),
            _ => Self::Text(Vec::new()),
        }
    }
}
pub(super) fn map<const N: usize>(values: [(&str, Meta); N]) -> Meta {
    Meta::Map(values.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
pub(super) fn int(value: impl Into<i64>) -> Meta {
    Meta::Int(value.into())
}

pub(super) fn descriptor<R: Read + Seek>(r: &mut Reader<R>, depth: usize) -> Result<Meta> {
    if depth > 64 {
        return Err(Error::Message("PSD descriptor nesting limit exceeded"));
    }
    r.unicode()?;
    r.id()?;
    let count = r.count()?;
    let mut values = BTreeMap::new();
    for _ in 0..count {
        let key = r.id()?;
        let value = item(r, depth + 1)?;
        values.insert(key, value);
    }
    Ok(Meta::Map(values))
}
fn item<R: Read + Seek>(r: &mut Reader<R>, depth: usize) -> Result<Meta> {
    if depth > 64 {
        return Err(Error::Message("PSD descriptor nesting limit exceeded"));
    }
    Ok(match &r.key()? {
        b"Objc" | b"GlbO" => descriptor(r, depth + 1)?,
        b"VlLs" => {
            let count = r.count()?;
            let mut list = Vec::new();
            for _ in 0..count {
                list.push(item(r, depth + 1)?);
            }
            Meta::List(list)
        }
        b"doub" => Meta::Real(r.f64()?),
        b"UntF" => {
            r.key()?;
            Meta::Real(r.f64()?)
        }
        b"TEXT" => Meta::Text(r.unicode()?),
        b"long" => int(r.i32()?),
        b"bool" => int(i32::from(r.u8()? != 0)),
        b"enum" => {
            r.id()?;
            Meta::Text(r.id()?.encode_utf16().collect())
        }
        b"type" | b"GlbC" => {
            r.unicode()?;
            r.id()?;
            Meta::Void
        }
        b"alis" | b"tdta" => {
            let n = r.u32()?;
            r.skip(u64::from(n))?;
            Meta::Void
        }
        b"obj " => {
            for _ in 0..r.count()? {
                match &r.key()? {
                    b"prop" => {
                        r.unicode()?;
                        r.id()?;
                        r.id()?;
                    }
                    b"Clss" => {
                        r.unicode()?;
                        r.id()?;
                    }
                    b"Enmr" => {
                        r.unicode()?;
                        r.id()?;
                        r.id()?;
                        r.id()?;
                    }
                    b"rele" => {
                        r.unicode()?;
                        r.id()?;
                        r.i32()?;
                    }
                    b"Idnt" | b"indx" => {
                        r.i32()?;
                    }
                    b"name" => {
                        r.unicode()?;
                    }
                    _ => return Err(Error::Message("unsupported PSD descriptor reference")),
                }
            }
            Meta::Void
        }
        _ => return Err(Error::Message("unsupported PSD descriptor value")),
    })
}
