//! Reference author/license notices: reference-notices.txt.
//! Layer.drawQRCode from Kirikiri2 qrcode/main.cpp; portable symbol generation.
//! No process-locale conversion or writable image pointers.
use krkr_engine::{
    extensions,
    protocol::{
        graphics::Size,
        pixels::{Bytes, Pixels},
    },
};
use qrcodegen::{Mask, QrCode, QrCodeEcc, QrSegment, Version};
use std::sync::Arc;
use tjs_bind::{RestArgs, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, Value, value};
krkr_engine::native_plugin! {
    pub(crate) Qrcode {
        names: ["qrcode.dll", "qrcode.tpm"],
        classes: [], extensions: [("Layer", "drawQRCode", draw::CALL)],
    }
}
const CAPACITY_ERROR: &str = "データが存在しないか、容量をオーバーしています";
#[tjs_bind::function(resumable = true)]
fn draw(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    let input = crate::exports::arg(args, 0)?;
    let owner = Value::Obj(cx.this().into());
    extensions::layer_size(cx, owner)?;
    if let Value::Str(id) = input
        && cx.heap().string(id)?.len() > 7089
    {
        return capacity(cx);
    }
    let units = value::to_string_units(cx.heap(), input)?;
    let text = String::from_utf16(&units).map_err(|_| NativeError::Message("invalid QR text"))?;
    let integer = |i: usize, default| -> NativeResult<i64> {
        Ok(args
            .get(i)
            .filter(|v| !matches!(v, Value::Void))
            .map(|v| value::to_integer(cx.heap(), *v).map(|n| n as i32 as i64))
            .unwrap_or(Ok(default))?)
    };
    let level = integer(1, 0)?;
    let version = integer(2, 0)?;
    let extend = integer(3, 1)? != 0;
    let mask = integer(4, -1)?;
    if text.is_empty()
        || !(0..=3).contains(&level)
        || !(0..=40).contains(&version)
        || !(-1..=7).contains(&mask)
    {
        return capacity(cx);
    }
    let ecc = [
        QrCodeEcc::Low,
        QrCodeEcc::Medium,
        QrCodeEcc::Quartile,
        QrCodeEcc::High,
    ][level as usize];
    let min = Version::new(version.max(1) as u8);
    let max = if version == 0 || extend {
        Version::MAX
    } else {
        min
    };
    let mask = (mask >= 0).then(|| Mask::new(mask as u8));
    // Modern text uses UTF-8 with an explicit ECI. ASCII retains numeric/alphanumeric modes.
    // The old wcstombs buffer truncated multibyte input to the UTF-16 character count.
    let mut segments = QrSegment::make_segments(&text);
    if !text.is_ascii() {
        segments.insert(0, QrSegment::make_eci(26));
    }
    let Ok(qr) = QrCode::encode_segments_advanced(&segments, ecc, min, max, mask, false) else {
        return capacity(cx);
    };
    let width = (qr.size() + 8) as u32;
    let size = Size {
        width,
        height: width,
    };
    let budget = extensions::layer_pixel_budget(cx, owner)?;
    let mut data = Bytes::zeroed(width as usize * width as usize * 4, &budget)
        .map_err(|e| NativeError::Detail(e.to_string()))?;
    for y in 0..width {
        for x in 0..width {
            let color = if qr.get_module(x as i32 - 4, y as i32 - 4) {
                [0, 0, 0, 255]
            } else {
                [255; 4]
            };
            let offset = ((y * width + x) * 4) as usize;
            data.as_mut_slice()[offset..offset + 4].copy_from_slice(&color);
        }
    }
    let pixels = Arc::new(Pixels {
        size,
        main: Some(data),
        province: None,
    });
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string("setImageSize".encode_utf16().collect::<Vec<_>>()),
    );
    Ok(NativeStep::CallMember {
        object: owner,
        key,
        arguments: vec![Value::Int(width.into()); 2],
        continuation: flow::callback(Paint { owner, pixels }, |s, cx, _| {
            extensions::layer_patch_pixels(cx, s.owner, s.pixels)
        }),
    })
}
#[derive(tjs_bind::Trace)]
struct Paint {
    owner: Value,
    #[trace(skip = "Budgeted image bytes have no VM references")]
    pixels: Arc<Pixels>,
}
fn capacity(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    Ok(NativeStep::Return(Value::Str(cx.heap_mut().alloc_string(
        CAPACITY_ERROR.encode_utf16().collect::<Vec<_>>(),
    ))))
}
