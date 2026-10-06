use super::*;
use crate::{
    font::{
        self,
        tasks::{Action, Data},
    },
    io,
};
use krkr_protocol::{graphics::Command, text::Style};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, value};

pub(crate) fn font_settings<R>(
    shared: &Shared,
    id: LayerId,
    read: impl FnOnce(&krkr_protocol::text::Font) -> R,
) -> NativeResult<R> {
    Ok(read(&shared.borrow().record(id)?.font))
}
pub(crate) fn update_font(
    shared: &Shared,
    id: LayerId,
    f: impl FnOnce(&mut krkr_protocol::text::Font),
) -> NativeResult<()> {
    f(&mut shared.borrow_mut().record_mut(id)?.font);
    Ok(())
}
pub(crate) fn font_require_main(shared: &Shared, id: LayerId) -> NativeResult<()> {
    shared.borrow().record(id)?.image()?;
    Ok(())
}
pub(crate) fn font_link(heap: &mut Heap, input: Value) -> NativeResult<font::Link> {
    let owner = object_id(input)?;
    heap.with_native_state::<bindings::State, _>(owner, |state| {
        state.lease.as_ref().map(|lease| font::Link {
            layers: lease.shared.clone(),
            id: lease.id,
            owner,
        })
    })?
    .ok_or(NativeError::This)
}
struct Drawn {
    shared: Shared,
    id: LayerId,
    style: Style,
    delivery: io::Delivery,
}
impl Trace for Drawn {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(r) = self.shared.borrow().records.get(self.id) {
            r.owner.trace(visit);
            r.action_owner.trace(visit);
        }
    }
}
impl NativeContinuation for Drawn {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Font(Data::Run(run))) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected text response"));
        };
        let (image, clip) = {
            let world = self.shared.borrow();
            let r = world.record(self.id)?;
            (r.image()?.clone(), r.geometry.clip)
        };
        tasks::request(
            &self.shared,
            self.id,
            Command::Text {
                image,
                run,
                style: self.style,
                clip,
            },
            tasks::Change::None,
            None,
        )
    }
}
impl bindings::State {
    pub(super) fn font(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
        if let Some(id) = self.read(|r| r.font_object)? {
            return Ok(object(id));
        }
        let lease = self.lease()?;
        let service = font::service(cx)?;
        let id = font::linked(
            cx.heap_mut(),
            service,
            font::Link {
                layers: lease.shared.clone(),
                id: lease.id,
                owner: self.read(|r| r.owner)?,
            },
        )?;
        lease.shared.borrow_mut().record_mut(lease.id)?.font_object = Some(id);
        Ok(object(id))
    }
    pub(super) fn draw_text(
        &self,
        cx: &mut NativeCx<'_>,
        args: &[Value],
    ) -> NativeResult<NativeStep> {
        if args.len() < 4 {
            return Err(NativeError::Message(
                "drawText requires x, y, text and color",
            ));
        }
        let (font, face, hold_alpha) = self.read(|r| (r.font.clone(), r.face(), r.hold_alpha))?;
        let face = face?;
        self.require_main()?;
        if matches!(face, DrawFace::Mask | DrawFace::Province) {
            return Err(NativeError::Message("drawText requires a main draw face"));
        }
        let int = |i, default| drawing::optional_integer(cx, args, i, default);
        let opacity = int(4, 255)?.clamp(-255, 255) as i16;
        if opacity < 0 && face == DrawFace::AddAlpha {
            return Err(NativeError::Message(
                "negative opacity is not supported on additive alpha",
            ));
        }
        let style = Style {
            color: crate::color::actual(bindings::integer(cx, args[3])? as u32),
            opacity: if face == DrawFace::Opaque {
                opacity.max(0)
            } else {
                opacity
            },
            antialias: args
                .get(5)
                .filter(|v| !matches!(v, Value::Void))
                .map_or(Ok(true), |v| v.truthy(cx.heap()))?,
            shadow_level: int(6, 0)?,
            shadow_color: crate::color::actual(int(7, 0)? as u32),
            shadow_width: int(8, 0)?,
            shadow_offset: [int(9, 0)?, int(10, 0)?],
            face,
            hold_alpha,
        };
        if style.opacity == 0 {
            return Ok(NativeStep::Return(Value::Void));
        }
        let x = bindings::integer(cx, args[0])?;
        let y = bindings::integer(cx, args[1])?;
        let Value::Str(text) = value::to_string(cx.heap_mut(), args[2])? else {
            unreachable!()
        };
        let text = krkr_assets::name::c_string(cx.heap().string(text)?).to_vec();
        let service = font::service(cx)?;
        let work = font::tasks::work(cx, &service, font, Action::Draw(text, style, x, y))?;
        let lease = self.lease()?;
        let delivery = io::Delivery::default();
        font::tasks::execute(
            cx,
            &service,
            work,
            delivery.clone(),
            Box::new(Drawn {
                shared: lease.shared.clone(),
                id: lease.id,
                style,
                delivery,
            }),
        )
    }
}
