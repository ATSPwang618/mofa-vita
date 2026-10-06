use super::{
    bindings::{State, integer, layer_id, rectangle},
    tasks::Change,
    *,
};
use krkr_protocol::graphics::{BlendOptions, Command};
use tjs_core::{NativeCx, NativeStep};

pub(super) fn optional_integer(
    cx: &NativeCx<'_>,
    args: &[Value],
    index: usize,
    default: i32,
) -> NativeResult<i32> {
    match args.get(index) {
        None | Some(Value::Void) => Ok(default),
        Some(value) => integer(cx, *value),
    }
}
impl State {
    pub(super) fn drawing_source(
        &self,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<super::source::Source> {
        let lease = self.lease()?;
        let id = if object_id(value)? == cx.this() {
            lease.id
        } else {
            match layer_id(cx.heap_mut(), value) {
                Ok(id) => id,
                Err(NativeError::This) => {
                    return Ok(super::source::Source::bitmap(
                        &lease.shared,
                        crate::bitmap::snapshot(cx.heap_mut(), value)?,
                    ));
                }
                Err(error) => return Err(error),
            }
        };
        let world = lease.shared.borrow();
        let record = world.record(id)?;
        Ok(super::source::Source::layer(
            record.image.clone(),
            record.blend,
        ))
    }
    pub(super) fn color(&self, cx: &NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
        if args.len() < 5 {
            return Err(NativeError::Message(
                "colorRect requires a rectangle and color",
            ));
        }
        let rect = rectangle(cx, args)?;
        let color = integer(cx, args[4])? as u32;
        let opacity = optional_integer(cx, args, 5, 255)?.clamp(-255, 255) as i16;
        let Some(rectangle) = rect.intersection(self.read(|r| r.geometry.clip)?) else {
            return Ok(NativeStep::Return(Value::Void));
        };
        let face = self.read(|r| r.face())??;
        if matches!(face, DrawFace::Mask | DrawFace::Province) {
            return self.fill(rectangle, color, Some(face));
        }
        let color = crate::color::actual(color);
        let image = self.image()?;
        if face == DrawFace::AddAlpha && opacity < 0 {
            return Err(NativeError::Message(
                "negative opacity is not supported on additive alpha",
            ));
        }
        if opacity == 0 {
            return Ok(NativeStep::Return(Value::Void));
        }
        self.command(
            Command::Color {
                image,
                rectangle,
                color,
                opacity,
                face,
            },
            Change::None,
        )
    }
    pub(super) fn operate(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
    ) -> NativeResult<NativeStep> {
        if args.len() < 7 {
            return Err(NativeError::Message(
                "operateRect requires destination, source layer and source rectangle",
            ));
        }
        let source_data = self.drawing_source(cx, args[2])?;
        let source = source_data.image()?;
        let mode = match optional_integer(cx, args, 7, 128)? {
            128 => source_data.mode,
            value => Blend::from_legacy(value)
                .ok_or(NativeError::Message("unknown image operation mode"))?,
        };
        let opacity = optional_integer(cx, args, 8, 255)?.clamp(0, 255) as u8;
        let x = integer(cx, args[0])?;
        let y = integer(cx, args[1])?;
        let rectangle = rectangle(cx, &args[3..])?;
        let clip = self.read(|r| r.geometry.clip)?;
        if (Rect {
            left: x,
            top: y,
            ..rectangle
        })
        .intersection(clip)
        .is_none()
        {
            return Ok(NativeStep::Return(Value::Void));
        }
        let options = self.read(|r| {
            r.face().map(|face| BlendOptions {
                mode,
                face,
                opacity,
                hold_alpha: r.hold_alpha,
            })
        })??;
        if !options.accepts_face() {
            return Err(NativeError::Message(
                "operation is not supported on this draw face",
            ));
        }
        let image = self.image()?;
        if options.is_noop() {
            return Ok(NativeStep::Return(Value::Void));
        }
        source_data.command(
            self,
            Command::Operate {
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                options,
            },
            Change::None,
        )
    }
}
