use krkr_engine::assets;
use std::{io, path::PathBuf};
use tjs_core::source::MAX_SOURCE_UNITS;

pub enum Input {
    Expression(String),
    File(PathBuf),
}

pub enum Content {
    Source(Vec<u16>),
    Bytecode(Vec<u8>),
}

pub fn read(input: &Input) -> Result<(String, Content), String> {
    match input {
        Input::Expression(text) => {
            if text.encode_utf16().count() > MAX_SOURCE_UNITS {
                return Err("source argument exceeds the UTF-16 size limit".into());
            }
            Ok((
                "<eval>".into(),
                Content::Source(text.encode_utf16().collect()),
            ))
        }
        Input::File(path) => {
            let read = || -> assets::Result<Vec<u8>> {
                let limits = assets::Limits {
                    max_read_bytes: MAX_SOURCE_UNITS * 3 + 3,
                    ..Default::default()
                };
                let mut vfs = assets::Vfs::new(&std::env::current_dir()?, limits)?;
                vfs.plan(&assets::local::units(path)?)?.read(0)
            };
            let bytes = read().map_err(|error| format!("{}: {error}", path.display()))?;
            if tjs_runtime::bytecode::is_bytecode(&bytes) {
                return Ok((path.display().to_string(), Content::Bytecode(bytes)));
            }
            let units =
                decode_source(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
            Ok((path.display().to_string(), Content::Source(units)))
        }
    }
}

fn decode_source(bytes: &[u8]) -> Result<Vec<u16>, io::Error> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        return Err(invalid("UTF-32 source is not implemented"));
    }
    let units = assets::text::decode(
        bytes,
        &assets::name::units("utf-8"),
        MAX_SOURCE_UNITS * 3 + 3,
    )
    .map_err(io::Error::other)?;
    if units.len() > MAX_SOURCE_UNITS {
        return Err(invalid("source exceeds the UTF-16 size limit"));
    }
    Ok(units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_preserves_code_units_and_rejects_ambiguous_input() {
        assert_eq!(decode_source(&[0xff, 0xfe, 0x00, 0xd8]).unwrap(), [0xd800]);
        assert_eq!(decode_source(&[0xfe, 0xff, 0x00, 0x31]).unwrap(), [49]);
        assert_eq!(decode_source(&[0xef, 0xbb, 0xbf, 49]).unwrap(), [49]);
        assert!(decode_source(&[0xff, 0xfe, 49]).is_err());
        assert!(decode_source(&[0x81, 0x40]).is_err());
        assert!(decode_source(&[0xff, 0xfe, 0, 0, 49, 0, 0, 0]).is_err());
    }
}
