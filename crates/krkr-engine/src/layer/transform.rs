use super::{
    bindings::{State, integer, rectangle},
    drawing::optional_integer,
    tasks::Change,
    *,
};
use krkr_protocol::{
    graphics::{BlendOptions, Command},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use tjs_core::{NativeCx, NativeStep, value};

fn sampling(
    cx: &NativeCx<'_>,
    args: &[Value],
    index: usize,
    cubic_option: bool,
) -> NativeResult<Sampling> {
    let legacy = optional_integer(cx, args, index, 0)?;
    let filter =
        Filter::from_legacy(legacy).ok_or(NativeError::Message("unknown image sampling type"))?;
    let sharpness = if cubic_option && args.len() > index + 1 {
        value::to_real(cx.heap(), args[index + 1])? as f32
    } else if matches!(legacy, 3 | 5) {
        -1.0
    } else {
        0.0
    };
    if !sharpness.is_finite() {
        return Err(NativeError::Message("filter sharpness must be finite"));
    }
    Ok(Sampling {
        filter,
        sharpness,
        no_clip: legacy & 0x10000 != 0,
    })
}
impl State {
    pub(super) fn affine_pile_legacy(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
    ) -> NativeResult<NativeStep> {
        self.legacy_blend(cx, args, 12, true)
    }
    pub(super) fn legacy_blend(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
        required: usize,
        pixel_alpha: bool,
    ) -> NativeResult<NativeStep> {
        if args.len() < required {
            return Err(NativeError::Message("not enough drawing arguments"));
        }
        if !matches!(
            self.read(|r| r.face())??,
            DrawFace::Alpha | DrawFace::Opaque
        ) {
            return Err(NativeError::Message(
                "legacy blend requires alpha or opaque draw face",
            ));
        }
        // Old pile methods use pixel alpha, irrespective of additive/PS layer
        // blend modes. Additive alpha is the only different source encoding.
        let mode = if !pixel_alpha {
            Blend::Opaque
        } else {
            let source = args[match required {
                7 => 2,
                9 => 4,
                _ => 0,
            }];
            if self.drawing_source(cx, source)?.mode == Blend::AddAlpha {
                Blend::AddAlpha
            } else {
                Blend::Alpha
            }
        };
        let optional = (args.len() - required).min(3);
        let mut operation = [Value::Void; 16];
        operation[..required].copy_from_slice(&args[..required]);
        operation[required] = Value::Int(mode as i64);
        operation[required + 1..required + 1 + optional]
            .copy_from_slice(&args[required..required + optional]);
        let operation = &operation[..required + 1 + optional];
        match required {
            7 => self.operate(cx, operation),
            9 => self.stretch(cx, operation, true),
            _ => self.affine(cx, operation, true),
        }
    }
    fn transform_operation(
        &self,
        cx: &NativeCx<'_>,
        args: &[Value],
        index: usize,
        source_mode: Blend,
        operate: bool,
    ) -> NativeResult<ImageOperation> {
        let (face, hold_alpha) = self.read(|r| (r.face(), r.hold_alpha))?;
        let face = face?;
        if !operate {
            if matches!(face, DrawFace::Mask | DrawFace::Province) {
                return Err(NativeError::Message(
                    "transformed copy requires a main draw face",
                ));
            }
            return Ok(ImageOperation::Copy {
                hold_alpha: face == DrawFace::Opaque && hold_alpha,
            });
        }
        let mode = match optional_integer(cx, args, index, 128)? {
            128 => source_mode,
            mode => Blend::from_legacy(mode)
                .ok_or(NativeError::Message("unknown image operation mode"))?,
        };
        let opacity = optional_integer(cx, args, index + 1, 255)?.clamp(0, 255) as u8;
        let options = BlendOptions {
            mode,
            face,
            hold_alpha,
            opacity,
        };
        if !options.accepts_face() {
            return Err(NativeError::Message(
                "operation is not supported on this draw face",
            ));
        }
        Ok(ImageOperation::Blend(options))
    }
    pub(super) fn stretch(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
        operate: bool,
    ) -> NativeResult<NativeStep> {
        if args.len() < 9 {
            return Err(NativeError::Message(
                "stretch requires destination, source layer and source rectangle",
            ));
        }
        let source_data = self.drawing_source(cx, args[4])?;
        let source = source_data.image()?;
        let source_mode = source_data.mode;
        let dest = StretchRect {
            left: integer(cx, args[0])?,
            top: integer(cx, args[1])?,
            width: integer(cx, args[2])?,
            height: integer(cx, args[3])?,
        };
        let rect = rectangle(cx, &args[5..])?;
        let sampling = sampling(cx, args, if operate { 11 } else { 9 }, true)?;
        let clip = self.read(|r| r.geometry.clip)?;
        let left = i64::from(dest.left).min(i64::from(dest.left) + i64::from(dest.width));
        let top = i64::from(dest.top).min(i64::from(dest.top) + i64::from(dest.height));
        let right = i64::from(dest.left).max(i64::from(dest.left) + i64::from(dest.width));
        let bottom = i64::from(dest.top).max(i64::from(dest.top) + i64::from(dest.height));
        if left >= i64::from(clip.left) + i64::from(clip.width)
            || right <= i64::from(clip.left)
            || top >= i64::from(clip.top) + i64::from(clip.height)
            || bottom <= i64::from(clip.top)
        {
            return Ok(NativeStep::Return(Value::Void));
        }
        let operation = self.transform_operation(cx, args, 9, source_mode, operate)?;
        source_data.command(
            self,
            Command::Transform {
                image: self.image()?,
                source,
                rectangle: rect,
                transform: Transform::Stretch(dest),
                sampling,
                operation,
                clip,
                clear: None,
            },
            Change::None,
        )
    }
    pub(super) fn affine(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
        operate: bool,
    ) -> NativeResult<NativeStep> {
        if args.len() < 12 {
            return Err(NativeError::Message(
                "affine requires source, rectangle and a matrix or three points",
            ));
        }
        let source_data = self.drawing_source(cx, args[0])?;
        let source = source_data.image()?;
        let source_mode = source_data.mode;
        let rect = rectangle(cx, &args[1..])?;
        let mut coordinates = [0.0; 6];
        for (out, input) in coordinates.iter_mut().zip(&args[6..12]) {
            *out = value::to_real(cx.heap(), *input)?;
        }
        let points = if args[5].truthy(cx.heap())? {
            let [a, b, c, d, tx, ty] = coordinates;
            let apply = |x: f64, y: f64| [a * x + c * y + tx, b * x + d * y + ty];
            [
                apply(-0.5, -0.5),
                apply(f64::from(rect.width) - 0.5, -0.5),
                apply(-0.5, f64::from(rect.height) - 0.5),
            ]
        } else {
            [
                [coordinates[0], coordinates[1]],
                [coordinates[2], coordinates[3]],
                [coordinates[4], coordinates[5]],
            ]
        };
        let operation = self.transform_operation(cx, args, 12, source_mode, operate)?;
        let sampling = sampling(cx, args, if operate { 14 } else { 12 }, false)?;
        let clear = if !operate && optional_integer(cx, args, 13, 0)? != 0 {
            Some(self.read(|r| r.neutral())?)
        } else {
            None
        };
        source_data.command(
            self,
            Command::Transform {
                image: self.image()?,
                source,
                rectangle: rect,
                transform: Transform::Affine(points),
                sampling,
                operation,
                clip: self.read(|r| r.geometry.clip)?,
                clear,
            },
            Change::None,
        )
    }
}
