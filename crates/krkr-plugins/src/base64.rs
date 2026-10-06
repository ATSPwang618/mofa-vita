//! kirikiri2@9b54995e, plugins/win32/base64/main.cpp. File conversion, not
//! Octet conversion. Keep its byte-table decoder, including non-alphabet bytes.
use crate::exports::arg;
use ::base64::{Engine as _, engine::general_purpose::STANDARD};
use krkr_engine::{
    assets::{Stream, local},
    storages,
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
};
use tjs_bind::{IntoTjs, RestArgs, Utf16, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, StrId, Value, value};

const CHUNK: usize = 12288; // Original stream block; a multiple of three.

krkr_engine::native_plugin! {
    pub(crate) Base64 {
        names: ["base64.dll", "base64.tpm"],
        classes: [bindings],
        extensions: [],
    }
}
#[tjs_bind::class(name = "Base64", static_class = true)]
mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> NativeResult<Self> {
            Err(NativeError::Message("Base64 cannot be instantiated"))
        }
        #[tjs::method]
        fn finalize() {}
        #[tjs::method(resumable = true)]
        fn encode(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            super::encode(cx, args)
        }
        #[tjs::method(resumable = true)]
        fn decode(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            super::decode(cx, args)
        }
    }
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
#[derive(tjs_bind::Trace)]
struct Encode {
    #[trace(skip = "Owned VM-free file/archive stream, closed when continuation drops")]
    input: Box<dyn Stream>,
    #[trace(
        skip = "UTF-16 code units contain no TJS handles; do not scan file contents during GC"
    )]
    output: Vec<u16>,
    remaining: u64,
}
fn encode(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args, 0)?;
    if !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    let path = value::to_string_units(cx.heap(), input)?;
    let limit = storages::service(cx)?.borrow().limits().max_read_bytes as u64;
    storages::managed::plans(cx, vec![(path, false)], limit, |limit, _, mut plans| {
        let Some(plan) = plans.pop().flatten() else {
            return Ok(NativeStep::Return(Value::Void));
        };
        // Account for the retained UTF-16 output as well as the input. Do not
        // reserve a whole input copy or permit unbounded managed-string growth.
        let output_bytes = plan.bytes.div_ceil(3).saturating_mul(8);
        if output_bytes > limit {
            return Err(NativeError::Message("Base64 exceeds storage byte budget"));
        }
        let mut output = Vec::new();
        output
            .try_reserve_exact((output_bytes / 2) as usize)
            .map_err(error)?;
        Ok(flow::work(
            Encode {
                input: plan.open().map_err(error)?,
                output,
                remaining: plan.bytes,
            },
            |s, cx| {
                let mut bytes = [0; CHUNK];
                let count = (s.remaining.min(CHUNK as u64)) as usize;
                if count != 0 {
                    // Read implementations may split blocks arbitrarily; padding is
                    // emitted only for the file's final group, never a short read.
                    s.input.read_exact(&mut bytes[..count]).map_err(error)?;
                    s.output
                        .extend(STANDARD.encode(&bytes[..count]).encode_utf16());
                    s.remaining -= count as u64;
                    return Ok(None);
                }
                let result = Utf16(std::mem::take(&mut s.output)).into_tjs(cx.heap_mut())?;
                Ok(Some(NativeStep::Return(result)))
            },
        ))
    })
}
#[derive(tjs_bind::Trace)]
struct Decode {
    input: StrId,
    position: usize,
    written: usize,
    limit: usize,
    #[trace(skip = "Owned output file contains no managed handles; drop closes partial writes")]
    output: File,
    #[trace(skip = "Shared VFS stores no TJS values")]
    vfs: storages::Shared,
    #[trace(skip = "MD5 state is a fixed Rust byte accumulator")]
    hash: Option<md5::Context>,
}
fn decode(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let destination = arg(args, 1)?;
    let Value::Str(input) = args[0] else {
        return Err(NativeError::Type("a Base64 String"));
    };
    let path = match destination {
        Value::Str(id) => cx.heap().string(id)?.to_vec(),
        Value::Void => Vec::new(),
        _ => return Err(NativeError::Type("a String filename")),
    };
    if path.is_empty() {
        return Err(NativeError::Message("no filename"));
    }
    let vfs = storages::service(cx)?;
    let full = vfs.borrow().full_path(&path).map_err(error)?;
    let path = local::resolve(&local::from_storage(&full).map_err(error)?).map_err(error)?;
    let limit = vfs.borrow().limits().max_read_bytes;
    let output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(error)?;
    vfs.borrow_mut().clear_archive_cache();
    Ok(flow::work(
        Decode {
            input,
            position: 0,
            written: 0,
            limit,
            output,
            vfs,
            hash: cx.result_needed().then(md5::Context::new),
        },
        decode_chunk,
    ))
}
fn sextet(unit: u16) -> NativeResult<u8> {
    Ok(match unit {
        65..=90 => (unit - 65) as u8,
        97..=122 => (unit - 97 + 26) as u8,
        48..=57 => (unit - 48 + 52) as u8,
        43 => 62,
        47 => 63,
        0..=255 => 0, // Including whitespace and embedded NUL, not ignored.
        _ => {
            return Err(NativeError::Message(
                "Base64 character outside the byte table",
            ));
        }
    })
}
fn decode_chunk(s: &mut Decode, cx: &mut NativeCx<'_>) -> NativeResult<Option<NativeStep>> {
    let input = cx.heap().string(s.input)?;
    if s.position >= input.len() {
        s.output.flush().map_err(error)?;
        let result = match s.hash.take() {
            Some(hash) => format!("{:x}", hash.finalize()).into_tjs(cx.heap_mut())?,
            None => Value::Void,
        };
        return Ok(Some(NativeStep::Return(result)));
    }
    let mut bytes = Vec::with_capacity(CHUNK);
    while bytes.len() < CHUNK && s.position < input.len() {
        let final_group = input.len() - s.position <= 4;
        let unit = |offset: usize| -> NativeResult<u16> {
            let index = s.position + offset;
            if index == input.len() {
                return Ok(0);
            } // C-string terminator.
            input
                .get(index)
                .copied()
                .ok_or(NativeError::Message("incomplete Base64 group"))
        };
        let a = sextet(unit(0)?)?;
        let b = sextet(unit(1)?)?;
        let c = unit(2)?;
        bytes.push((a << 2) | (b >> 4));
        if !final_group || c != 61 {
            let c = sextet(c)?;
            bytes.push((b << 4) | (c >> 2));
            let d = unit(3)?;
            if !final_group || d != 61 {
                bytes.push((c << 6) | sextet(d)?);
            }
        }
        s.position += 4;
    }
    if bytes.len() > s.limit.saturating_sub(s.written) {
        return Err(NativeError::Message("Base64 exceeds storage byte budget"));
    }
    s.output.write_all(&bytes).map_err(error)?;
    s.vfs.borrow_mut().clear_archive_cache();
    s.written += bytes.len();
    if let Some(hash) = &mut s.hash {
        hash.consume(&bytes);
    }
    Ok(None)
}
