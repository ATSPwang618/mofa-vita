use super::{
    tasks::{Change, request},
    *,
};
pub(super) use implementation::State;
use krkr_protocol::graphics::Command;
use tjs_core::{NativeCx, NativeStep, RestArgs, value};
pub(super) fn integer(cx: &NativeCx<'_>, value: Value) -> NativeResult<i32> {
    Ok(value::to_integer(cx.heap(), value)? as i32)
}
pub(super) fn size(width: i32, height: i32) -> NativeResult<Size> {
    if width <= 0 || height <= 0 {
        return Err(NativeError::Message("image dimensions must be positive"));
    }
    Ok(Size {
        width: width as u32,
        height: height as u32,
    })
}
pub(super) fn rectangle(cx: &NativeCx<'_>, args: &[Value]) -> NativeResult<Rect> {
    if args.len() < 4 {
        return Err(NativeError::Message(
            "four rectangle arguments are required",
        ));
    }
    Ok(Rect {
        left: integer(cx, args[0])?,
        top: integer(cx, args[1])?,
        width: integer(cx, args[2])?.max(0) as u32,
        height: integer(cx, args[3])?.max(0) as u32,
    })
}
pub(super) fn layer_id(heap: &mut Heap, value: Value) -> NativeResult<LayerId> {
    heap.with_native_state::<State, _>(object_id(value)?, |state| {
        state.lease.as_ref().map(|l| l.id)
    })?
    .ok_or(NativeError::This)
}

#[tjs_bind::class(name = "Layer")]
mod implementation {
    use super::super::input::{Returned, changes::Operation, focus, keyboard};
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        pub lease: Option<Lease>,
        pub pending_name: Vec<u16>,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            if let Some(service) = &self.service {
                let service = service.borrow();
                service.trace_transitions(visit);
                for record in service.records.values().filter(|r| r.paint_queued) {
                    record.owner.trace(visit);
                }
                for value in service.names.values() {
                    value.trace(visit);
                }
            }
            if let Some(lease) = &self.lease
                && let Some(record) = lease.shared.borrow().records.get(lease.id)
            {
                record.action_owner.trace(visit);
                record.children_array.trace(visit);
                record.font_object.trace(visit);
            }
        }
    }
    impl State {
        #[tjs::method(name = "copyToBitmapFromMainImage", resumable = true)]
        fn copy_to_bitmap(
            &self,
            cx: &mut NativeCx<'_>,
            destination: Value,
        ) -> NativeResult<NativeStep> {
            self.main_to_bitmap(cx, destination)
        }
        #[tjs::method(name = "copyFromBitmapToMainImage", resumable = true)]
        fn copy_from_bitmap(
            &self,
            cx: &mut NativeCx<'_>,
            source: Value,
        ) -> NativeResult<NativeStep> {
            self.bitmap_to_main(cx, source)
        }
        #[tjs::method(name = "independMainImage", resumable = true)]
        fn independ_main(&self, cx: &NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.independ_image(cx, args, false)
        }
        #[tjs::method(name = "independProvinceImage", resumable = true)]
        fn independ_province(
            &self,
            cx: &NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.independ_image(cx, args, true)
        }
        #[tjs::method(name = "adjustGamma", resumable = true)]
        fn adjust_gamma(&self, cx: &NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.is_empty() {
                return Ok(NativeStep::Return(Value::Void));
            }
            self.require_main()?;
            let mut parameters = [(1.0f32, 0i32, 255i32); 3];
            for (channel, (gamma, floor, ceil)) in parameters.iter_mut().enumerate() {
                let offset = channel * 3;
                if let Some(v) = args.get(offset).filter(|v| !matches!(v, Value::Void)) {
                    *gamma = value::to_real(cx.heap(), *v)? as f32;
                }
                if let Some(v) = args.get(offset + 1).filter(|v| !matches!(v, Value::Void)) {
                    *floor = integer(cx, *v)?;
                }
                if let Some(v) = args.get(offset + 2).filter(|v| !matches!(v, Value::Void)) {
                    *ceil = integer(cx, *v)?;
                }
                if !gamma.is_finite() || *gamma < 0.0 {
                    return Err(NativeError::Message("gamma must be finite and nonnegative"));
                }
            }
            if parameters == [(1.0, 0, 255); 3] {
                self.update(|r| {
                    r.image_modified = true;
                    Ok(())
                })?;
                return Ok(NativeStep::Return(Value::Void));
            }
            let table = std::array::from_fn(|index| {
                let mut row = [0u32; 4];
                for (channel, &(gamma, floor, ceil)) in parameters.iter().enumerate() {
                    // Legacy games use zero gamma. The original exp/log LUT
                    // maps sub-white samples to the floor. Define its non-finite
                    // white endpoint as black, as in our saturating LUT cast,
                    // instead of propagating NaN into renderer state.
                    if gamma == 0.0 {
                        row[channel] = if index == 255 {
                            0
                        } else {
                            floor.clamp(0, 255) as u32
                        };
                        continue;
                    }
                    let value = ((index as f64 / 255.0).ln() / f64::from(gamma)).exp()
                        * (f64::from(ceil) - f64::from(floor))
                        + 0.5
                        + f64::from(floor);
                    row[channel] = value.clamp(0.0, 255.0) as u32;
                }
                row
            });
            let additive = self.read(|r| r.face())?? == DrawFace::AddAlpha;
            self.adjust(krkr_protocol::graphics::Adjustment::Gamma {
                table: Arc::new(table),
                additive,
            })
        }
        #[tjs::method(name = "doGrayScale", resumable = true)]
        fn gray_scale(&self) -> NativeResult<NativeStep> {
            self.adjust(krkr_protocol::graphics::Adjustment::GrayScale)
        }
        #[tjs::method(name = "doBoxBlur", resumable = true)]
        fn box_blur(&self, cx: &NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let radius = [
                drawing::optional_integer(cx, args, 0, 1)?.unsigned_abs(),
                drawing::optional_integer(cx, args, 1, 1)?.unsigned_abs(),
            ];
            self.require_main()?;
            if self
                .read(|r| r.geometry.clip.intersection(r.geometry.image_size.rect()))?
                .is_none()
            {
                return Ok(NativeStep::Return(Value::Void));
            }
            let area = (u64::from(radius[0]) * 2 + 1).checked_mul(u64::from(radius[1]) * 2 + 1);
            if area.is_none_or(|area| area >= 1 << 24) {
                return Err(NativeError::Message(
                    "box blur area must be smaller than 16 million pixels",
                ));
            }
            if radius == [0, 0] {
                return Ok(NativeStep::Return(Value::Void));
            }
            self.adjust(krkr_protocol::graphics::Adjustment::BoxBlur {
                radius,
                alpha: self.read(|r| r.face())?? == DrawFace::Alpha,
            })
        }
        #[tjs::method(name = "flipLR", resumable = true)]
        fn flip_lr(&self) -> NativeResult<NativeStep> {
            self.adjust(krkr_protocol::graphics::Adjustment::Flip { horizontal: true })
        }
        #[tjs::method(name = "flipUD", resumable = true)]
        fn flip_ud(&self) -> NativeResult<NativeStep> {
            self.adjust(krkr_protocol::graphics::Adjustment::Flip { horizontal: false })
        }
        #[tjs::getter(name = "imageModified")]
        fn image_modified(&self) -> NativeResult<bool> {
            self.read(|r| r.image_modified)
        }
        #[tjs::setter(name = "imageModified")]
        fn set_image_modified(&self, cx: &NativeCx<'_>, value: Value) -> NativeResult<()> {
            let modified = value.truthy(cx.heap())?;
            self.update(|r| {
                r.image_modified = modified;
                Ok(())
            })
        }
        #[tjs::getter(name = "cached")]
        fn cached(&self) -> NativeResult<bool> {
            self.read(|r| r.cache.is_some())
        }
        #[tjs::setter(name = "cached")]
        fn set_cached(&self, enabled: bool) -> NativeResult<()> {
            if self.read(|r| r.cache.is_some())? == enabled {
                return Ok(());
            }
            self.update(|r| {
                r.cache = enabled.then(|| Arc::new(()));
                Ok(())
            })
        }
        #[tjs::getter(name = "nodeVisible")]
        fn node_visible(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            let world = lease.shared.borrow();
            let mut node = Some(lease.id);
            while let Some(id) = node {
                let record = world.record(id)?;
                if !record.visible {
                    return Ok(false);
                }
                node = record.parent;
            }
            Ok(true)
        }
        #[tjs::constructor(resumable = true)]
        fn create(
            cx: &mut NativeCx<'_>,
            action_owner: Value,
            parent: Value,
        ) -> NativeResult<NativeStep> {
            let class = cx
                .heap()
                .registered_class("Layer")
                .expect("installed Layer");
            let shared = cx
                .heap_mut()
                .with_native_state::<State, _>(class, |s| s.service.clone())?
                .expect("Layer world");
            let window = crate::window::bindings::id(cx.heap_mut(), object_id(action_owner)?)?;
            let parent = if matches!(parent, Value::Obj(ObjRef { object: None, .. })) {
                None
            } else {
                Some(layer_id(cx.heap_mut(), parent)?)
            };
            let lease = Layers::create(&shared, cx.this(), action_owner, window, parent)?;
            let id = lease.id;
            let object = cx.this();
            let name = cx
                .heap_mut()
                .with_native_state::<State, _>(object, |s| std::mem::take(&mut s.pending_name))?;
            shared.borrow_mut().record_mut(id)?.name = name;
            let (image, size) = {
                let w = shared.borrow();
                let r = w.record(id)?;
                (r.image.clone().unwrap(), r.geometry.image_size)
            };
            request(
                &shared,
                id,
                Command::Create {
                    image: image.id,
                    lifetime: Arc::downgrade(&image.lifetime),
                    size,
                    color: 0x00ffffff,
                },
                Change::None,
                Some(lease),
            )
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::getter(name = "window")]
        fn window(&self) -> NativeResult<Value> {
            let lease = self.lease()?;
            let world = lease.shared.borrow();
            Ok(world
                .windows
                .borrow()
                .owner(world.record(lease.id)?.window)
                .map(object)
                .unwrap_or_else(null))
        }
        #[tjs::method(name = "getLayerAt", resumable = true)]
        fn get_layer_at(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
            options: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let coordinates = lease.shared.borrow().coordinates(lease.id);
            let Ok((window, ox, oy)) = coordinates else {
                return Ok(NativeStep::Return(null()));
            };
            let exclude = options.first().is_some_and(|v| !matches!(v, Value::Void))
                && options[0].truthy(cx.heap())?;
            let disabled = options.get(1).is_some_and(|v| !matches!(v, Value::Void))
                && options[1].truthy(cx.heap())?;
            Ok(super::super::hit::start(
                &lease.shared,
                window,
                i64::from(integer(cx, x)?) + ox,
                i64::from(integer(cx, y)?) + oy,
                exclude.then_some(lease.id),
                disabled,
                None,
            ))
        }
        #[tjs::method(name = "onHitTest")]
        fn on_hit_test(&self, _x: Value, _y: Value, hit: bool) -> NativeResult<()> {
            self.input_update(|r| {
                r.hit_work = hit;
            })
        }
        #[tjs::getter(name = "hitType")]
        fn hit_type(&self) -> NativeResult<i64> {
            self.read(|r| r.hit_type.into())
        }
        #[tjs::setter(name = "hitType")]
        fn set_hit_type(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let value = integer(cx, value)?;
            self.input_update(|r| {
                r.hit_type = value;
            })
        }
        #[tjs::getter(name = "hitThreshold")]
        fn hit_threshold(&self) -> NativeResult<i64> {
            self.read(|r| r.hit_threshold.into())
        }
        #[tjs::setter(name = "hitThreshold")]
        fn set_hit_threshold(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let value = integer(cx, value)?;
            self.input_update(|r| {
                r.hit_threshold = value;
            })
        }
        #[tjs::getter(name = "enabled")]
        fn enabled(&self) -> NativeResult<bool> {
            self.read(|r| r.enabled)
        }
        #[tjs::setter(name = "enabled", resumable = true)]
        fn set_enabled(&self, value: bool) -> NativeResult<NativeStep> {
            self.change_input(Operation::Enabled(value))
        }
        #[tjs::getter(name = "nodeEnabled")]
        fn node_enabled(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().node_enabled(lease.id))
        }
        #[tjs::method(name = "releaseCapture")]
        fn release_capture(&self) -> NativeResult<()> {
            let lease = self.lease()?;
            let mut world = lease.shared.borrow_mut();
            let window = world.record(lease.id)?.window;
            world.release_capture(window);
            Ok(())
        }
        #[tjs::method(name = "onClick", resumable = true)]
        fn on_click(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.event(cx, "onClick", &[2, 3], args)
        }
        #[tjs::method(name = "onDoubleClick", resumable = true)]
        fn on_double_click(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onDoubleClick", &[2, 3], args)
        }
        #[tjs::method(name = "onMouseDown", resumable = true)]
        fn on_mouse_down(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onMouseDown", &[2, 3, 4, 5], args)
        }
        #[tjs::method(name = "onMouseUp", resumable = true)]
        fn on_mouse_up(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onMouseUp", &[2, 3, 4, 5], args)
        }
        #[tjs::method(name = "onMouseMove", resumable = true)]
        fn on_mouse_move(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onMouseMove", &[2, 3, 5], args)
        }
        #[tjs::method(name = "onMouseEnter", resumable = true)]
        fn on_mouse_enter(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.event(cx, "onMouseEnter", &[], &[])
        }
        #[tjs::method(name = "onMouseLeave", resumable = true)]
        fn on_mouse_leave(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.event(cx, "onMouseLeave", &[], &[])
        }
        #[tjs::getter(name = "cursor")]
        fn cursor(&self) -> NativeResult<i64> {
            self.read(|r| r.cursor.into())
        }
        #[tjs::setter(name = "cursor", resumable = true)]
        fn set_cursor(&self, cx: &mut NativeCx<'_>, cursor: Value) -> NativeResult<NativeStep> {
            self.load_cursor(cx, cursor)
        }
        #[tjs::getter(name = "hint")]
        fn hint(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let text = self.read(|r| r.hint.clone())?;
            Ok(Value::Str(cx.heap_mut().alloc_string(text.to_vec())))
        }
        #[tjs::setter(name = "hint", resumable = true)]
        fn set_hint(&self, cx: &mut NativeCx<'_>, text: Value) -> NativeResult<NativeStep> {
            let Value::Str(text) = value::to_string(cx.heap_mut(), text)? else {
                unreachable!()
            };
            let text = Arc::from(tjs_core::string::c_string(cx.heap().string(text)?));
            self.input_update(|r| {
                r.hint = text;
                r.show_parent_hint = false;
                r.ignore_hint_sensing = false;
            })?;
            self.notify_hint()
        }
        #[tjs::getter(name = "showParentHint")]
        fn show_parent_hint(&self) -> NativeResult<bool> {
            self.read(|r| r.show_parent_hint)
        }
        #[tjs::setter(name = "showParentHint")]
        fn set_show_parent_hint(&self, cx: &mut NativeCx<'_>, flag: Value) -> NativeResult<()> {
            let flag = flag.truthy(cx.heap())?;
            self.input_update(|r| r.show_parent_hint = flag)
        }
        #[tjs::getter(name = "ignoreHintSensing")]
        fn ignore_hint_sensing(&self) -> NativeResult<bool> {
            self.read(|r| r.ignore_hint_sensing)
        }
        #[tjs::setter(name = "ignoreHintSensing")]
        fn set_ignore_hint_sensing(&self, cx: &mut NativeCx<'_>, flag: Value) -> NativeResult<()> {
            let flag = flag.truthy(cx.heap())?;
            self.input_update(|r| r.ignore_hint_sensing = flag)
        }
        #[tjs::getter(name = "imeMode")]
        fn ime_mode(&self) -> NativeResult<i64> {
            self.read(|r| r.ime.into())
        }
        #[tjs::setter(name = "imeMode", resumable = true)]
        fn set_ime_mode(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            let value = integer(cx, value)?;
            self.input_update(|r| r.ime = value)?;
            self.sync_ime(cx)
        }
        #[tjs::getter(name = "attentionLeft")]
        fn attention_left(&self) -> NativeResult<i64> {
            self.read(|r| r.attention.0.into())
        }
        #[tjs::setter(name = "attentionLeft", resumable = true)]
        fn set_attention_left(
            &self,
            cx: &mut NativeCx<'_>,
            value: Value,
        ) -> NativeResult<NativeStep> {
            let value = integer(cx, value)?;
            self.input_update(|r| r.attention.0 = value)?;
            self.sync_ime(cx)
        }
        #[tjs::getter(name = "attentionTop")]
        fn attention_top(&self) -> NativeResult<i64> {
            self.read(|r| r.attention.1.into())
        }
        #[tjs::setter(name = "attentionTop", resumable = true)]
        fn set_attention_top(
            &self,
            cx: &mut NativeCx<'_>,
            value: Value,
        ) -> NativeResult<NativeStep> {
            let value = integer(cx, value)?;
            self.input_update(|r| r.attention.1 = value)?;
            self.sync_ime(cx)
        }
        #[tjs::getter(name = "useAttention")]
        fn use_attention(&self) -> NativeResult<bool> {
            self.read(|r| r.use_attention)
        }
        #[tjs::setter(name = "useAttention", resumable = true)]
        fn set_use_attention(
            &self,
            cx: &mut NativeCx<'_>,
            value: bool,
        ) -> NativeResult<NativeStep> {
            self.input_update(|r| r.use_attention = value)?;
            self.sync_ime(cx)
        }
        #[tjs::method(name = "setAttentionPos", resumable = true)]
        fn set_attention_pos(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
        ) -> NativeResult<NativeStep> {
            let point = (integer(cx, x)?, integer(cx, y)?);
            self.input_update(|r| r.attention = point)?;
            self.sync_ime(cx)
        }
        #[tjs::getter(name = "cursorX")]
        fn cursor_x(&self) -> NativeResult<i64> {
            self.get_cursor_pos(true)
        }
        #[tjs::getter(name = "cursorY")]
        fn cursor_y(&self) -> NativeResult<i64> {
            self.get_cursor_pos(false)
        }
        #[tjs::setter(name = "cursorX")]
        fn set_cursor_x(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let x = integer(cx, value)?;
            self.input_update(|r| r.cursor_x = x)
        }
        #[tjs::setter(name = "cursorY", resumable = true)]
        fn set_cursor_y(&self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            self.move_cursor(self.read(|r| r.cursor_x)?, integer(cx, value)?)
        }
        #[tjs::method(name = "setCursorPos", resumable = true)]
        fn set_cursor_pos(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
        ) -> NativeResult<NativeStep> {
            self.move_cursor(integer(cx, x)?, integer(cx, y)?)
        }
        #[tjs::getter(name = "focusable")]
        fn focusable(&self) -> NativeResult<bool> {
            self.read(|r| r.focusable)
        }
        #[tjs::setter(name = "focusable", resumable = true)]
        fn set_focusable(&self, value: bool) -> NativeResult<NativeStep> {
            self.change_input(Operation::Focusable(value))
        }
        #[tjs::getter(name = "nextFocusable", resumable = true)]
        fn next_focusable(&self) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            Ok(focus::search(
                lease.shared.clone(),
                lease.id,
                true,
                Box::new(Returned),
            ))
        }
        #[tjs::getter(name = "prevFocusable", resumable = true)]
        fn prev_focusable(&self) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            Ok(focus::search(
                lease.shared.clone(),
                lease.id,
                false,
                Box::new(Returned),
            ))
        }
        #[tjs::getter(name = "joinFocusChain")]
        fn join_focus_chain(&self) -> NativeResult<bool> {
            self.read(|r| r.join_focus_chain)
        }
        #[tjs::setter(name = "joinFocusChain")]
        fn set_join_focus_chain(&self, value: bool) -> NativeResult<()> {
            self.input_update(|r| r.join_focus_chain = value)
        }
        #[tjs::getter(name = "nodeFocusable")]
        fn node_focusable(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().node_focusable(lease.id))
        }
        #[tjs::getter(name = "focused")]
        fn focused(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            let world = lease.shared.borrow();
            Ok(world.focused(world.record(lease.id)?.window) == Some(lease.id))
        }
        #[tjs::method(name = "focus", resumable = true)]
        fn focus(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let forward = args.first().map_or(Ok(true), |v| v.truthy(cx.heap()))?;
            let lease = self.lease()?;
            let window = lease.shared.borrow().record(lease.id)?.window;
            Ok(focus::set(
                lease.shared.clone(),
                window,
                Some(lease.id),
                forward,
                Box::new(Returned),
            ))
        }
        #[tjs::method(name = "focusNext", resumable = true)]
        fn focus_next(&self) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let window = lease.shared.borrow().record(lease.id)?.window;
            Ok(focus::navigate(
                lease.shared.clone(),
                window,
                true,
                Box::new(Returned),
            ))
        }
        #[tjs::method(name = "focusPrev", resumable = true)]
        fn focus_prev(&self) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let window = lease.shared.borrow().record(lease.id)?.window;
            Ok(focus::navigate(
                lease.shared.clone(),
                window,
                false,
                Box::new(Returned),
            ))
        }
        #[tjs::method(name = "setMode", resumable = true)]
        fn set_mode(&self) -> NativeResult<NativeStep> {
            self.change_input(Operation::SetMode)
        }
        #[tjs::method(name = "removeMode", resumable = true)]
        fn remove_mode(&self) -> NativeResult<NativeStep> {
            self.change_input(Operation::RemoveMode)
        }
        #[tjs::method(name = "onBeforeFocus", resumable = true)]
        fn on_before_focus(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let value = *args.first().ok_or(NativeError::Missing(1))?;
            self.event_then(
                cx,
                "onBeforeFocus",
                &[8, 9, 10],
                args,
                Box::new(focus::Selection {
                    shared: lease.shared.clone(),
                    id: lease.id,
                    value,
                }),
            )
        }
        #[tjs::method(name = "onSearchNextFocusable", resumable = true)]
        fn on_search_next_focusable(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let value = *args.first().ok_or(NativeError::Missing(1))?;
            self.event_then(
                cx,
                "onSearchNextFocusable",
                &[8],
                args,
                Box::new(focus::Selection {
                    shared: lease.shared.clone(),
                    id: lease.id,
                    value,
                }),
            )
        }
        #[tjs::method(name = "onSearchPrevFocusable", resumable = true)]
        fn on_search_prev_focusable(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            let value = *args.first().ok_or(NativeError::Missing(1))?;
            self.event_then(
                cx,
                "onSearchPrevFocusable",
                &[8],
                args,
                Box::new(focus::Selection {
                    shared: lease.shared.clone(),
                    id: lease.id,
                    value,
                }),
            )
        }
        #[tjs::method(name = "onBlur", resumable = true)]
        fn on_blur(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.event(cx, "onBlur", &[11], args)
        }
        #[tjs::method(name = "onFocus", resumable = true)]
        fn on_focus(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.event(cx, "onFocus", &[9, 10], args)
        }
        #[tjs::method(name = "onNodeEnabled", resumable = true)]
        fn on_node_enabled(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onNodeEnabled", &[], args)
        }
        #[tjs::method(name = "onNodeDisabled", resumable = true)]
        fn on_node_disabled(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onNodeDisabled", &[], args)
        }
        #[tjs::method(name = "onMouseWheel", resumable = true)]
        fn on_mouse_wheel(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onMouseWheel", &[5, 7, 2, 3], args)
        }
        #[tjs::method(name = "onKeyDown", resumable = true)]
        fn on_key_down(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.key_event(cx, keyboard::Kind::Down, args)
        }
        #[tjs::method(name = "onKeyUp", resumable = true)]
        fn on_key_up(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.key_event(cx, keyboard::Kind::Up, args)
        }
        #[tjs::method(name = "onKeyPress", resumable = true)]
        fn on_key_press(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.key_event(cx, keyboard::Kind::Press, args)
        }
        #[tjs::invalidate(resumable = true)]
        fn invalidate(&self) -> NativeResult<NativeStep> {
            self.change_input(Operation::Invalidate)
        }
        #[tjs::method(name = "beginTransition", resumable = true)]
        fn begin_transition(
            &self,
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let Value::Str(name) = value::to_string(cx.heap_mut(), name)? else {
                unreachable!()
            };
            let name =
                String::from_utf16_lossy(tjs_core::string::c_string(cx.heap().string(name)?));
            let with_children = args
                .first()
                .filter(|v| !matches!(v, Value::Void))
                .map_or(Ok(true), |v| v.truthy(cx.heap()))?;
            let source = layer_id(cx.heap_mut(), args.get(1).copied().unwrap_or_else(null))?;
            let options = args.get(2).copied().unwrap_or(Value::Void);
            let lease = self.lease()?;
            transition::begin::start(
                lease.shared.clone(),
                lease.id,
                source,
                &name,
                with_children,
                options,
            )
        }
        #[tjs::method(name = "stopTransition", resumable = true)]
        fn stop_transition(&self) -> NativeResult<NativeStep> {
            let lease = self.lease()?;
            transition::stop_layer(lease.shared.clone(), lease.id)
        }
        #[tjs::method(name = "onTransitionCompleted", resumable = true)]
        fn on_transition_completed(
            &self,
            cx: &mut NativeCx<'_>,
            dest: Value,
            src: Value,
        ) -> NativeResult<NativeStep> {
            self.event(cx, "onTransitionCompleted", &[13, 14], &[dest, src])
        }
        #[tjs::method(name = "update")]
        fn update_event(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            if !args.is_empty() {
                let _ = rectangle(cx, args)?;
            }
            self.update(|r| {
                r.call_on_paint = true;
                Ok(())
            })
        }
        #[tjs::getter(name = "callOnPaint")]
        fn call_on_paint(&self) -> NativeResult<bool> {
            self.read(|r| r.call_on_paint)
        }
        #[tjs::setter(name = "callOnPaint")]
        fn set_call_on_paint(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let flag = input.truthy(cx.heap())?;
            self.input_update(|r| r.call_on_paint = flag)
        }
        #[tjs::method(name = "onPaint", resumable = true)]
        fn on_paint(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.event(cx, "onPaint", &[], &[])
        }
        #[tjs::getter(name = "font")]
        fn get_font(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let cached = cx
                .heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.font_object)
                })??;
            if let Some(font) = cached {
                return Ok(object(font));
            }
            // First access installs a traced edge; ordinary reads must not
            // repeatedly dirty the layer during incremental collection.
            cx.with_state::<Self, _>(|state, cx| state.font(cx))
        }
        #[tjs::method(name = "drawText", resumable = true)]
        fn text(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.draw_text(cx, args)
        }
        #[tjs::method(name = "assignImages", resumable = true)]
        fn assign_images(&self, cx: &mut NativeCx<'_>, source: Value) -> NativeResult<NativeStep> {
            self.assign(cx, source)
        }
        #[tjs::getter(name = "neutralColor")]
        fn get_neutral_color(&self) -> NativeResult<i64> {
            self.read(|r| i64::from(r.neutral))
        }
        #[tjs::setter(name = "neutralColor")]
        fn set_neutral_color(&self, #[tjs(coerce)] color: i64) -> NativeResult<()> {
            self.update_if_changed(|r| {
                if r.neutral == color as u32 {
                    return Ok(false);
                }
                r.neutral = color as u32;
                Ok(true)
            })
        }
        #[tjs::getter(name = "hasImage")]
        fn get_has_image(&self) -> NativeResult<bool> {
            self.read(|r| r.has_main)
        }
        #[tjs::setter(name = "hasImage", resumable = true)]
        fn set_has_image(&self, value: bool) -> NativeResult<NativeStep> {
            self.has_image(value)
        }
        #[tjs::method(name = "loadImages", resumable = true)]
        fn load_images(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let name = args
                .first()
                .copied()
                .ok_or(NativeError::Message("loadImages requires a storage name"))?;
            let key = match args.get(1) {
                None | Some(Value::Void) => 0x1fffffff,
                Some(value) => integer(cx, *value)? as u32,
            };
            self.load(cx, name, key, false)
        }
        #[tjs::method(name = "saveLayerImage", resumable = true)]
        fn save_layer_image(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let name = args.first().copied().ok_or(NativeError::Message(
                "saveLayerImage requires a storage name",
            ))?;
            self.save(cx, name, args.get(1).copied().unwrap_or(Value::Void))
        }
        #[tjs::method(name = "loadProvinceImage", resumable = true)]
        fn load_province_image(
            &self,
            cx: &mut NativeCx<'_>,
            name: Value,
        ) -> NativeResult<NativeStep> {
            self.load(cx, name, 0, true)
        }
        #[tjs::method(name = "setSize", resumable = true)]
        fn set_size(
            &self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<NativeStep> {
            self.change_size(integer(cx, width)?, integer(cx, height)?, false)
        }
        #[tjs::method(name = "setImageSize", resumable = true)]
        fn set_image_size(
            &self,
            cx: &mut NativeCx<'_>,
            width: Value,
            height: Value,
        ) -> NativeResult<NativeStep> {
            self.change_size(integer(cx, width)?, integer(cx, height)?, true)
        }
        #[tjs::method(name = "setPos", resumable = true)]
        fn set_pos(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() < 2 {
                return Err(NativeError::Message("setPos requires left and top"));
            }
            if args.len() == 4 && !matches!(args[2], Value::Void) && !matches!(args[3], Value::Void)
            {
                let (width, height) = (integer(cx, args[2])?, integer(cx, args[3])?);
                if width < 0 || height < 0 {
                    return Err(NativeError::Message("layer bounds cannot be negative"));
                }
                self.move_to(integer(cx, args[0])?, integer(cx, args[1])?)?;
                self.change_size(width, height, false)
            } else {
                self.move_to(integer(cx, args[0])?, integer(cx, args[1])?)?;
                Ok(NativeStep::Return(Value::Void))
            }
        }
        #[tjs::method(name = "setSizeToImageSize", resumable = true)]
        fn set_size_to_image_size(&self) -> NativeResult<NativeStep> {
            self.require_main()?;
            let size = self.read(|r| r.geometry.image_size)?;
            self.change_size(size.width as i32, size.height as i32, false)
        }
        #[tjs::method(name = "setImagePos")]
        fn set_image_pos(&self, cx: &mut NativeCx<'_>, x: Value, y: Value) -> NativeResult<()> {
            let (x, y) = (integer(cx, x)?, integer(cx, y)?);
            self.update_if_changed(|r| {
                r.image()?;
                let g = &mut r.geometry;
                if x > 0
                    || y > 0
                    || i64::from(x) + i64::from(g.image_size.width) < i64::from(g.width)
                    || i64::from(y) + i64::from(g.image_size.height) < i64::from(g.height)
                {
                    return Err(NativeError::Message("image must cover the layer rectangle"));
                }
                if (g.image_left, g.image_top) == (x, y) {
                    return Ok(false);
                }
                g.image_left = x;
                g.image_top = y;
                Ok(true)
            })
        }
        #[tjs::method(name = "setClip")]
        fn set_clip(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<()> {
            self.require_main()?;
            if args.is_empty() {
                return self.update(|r| {
                    r.geometry.clip = r.geometry.image_size.rect();
                    Ok(())
                });
            }
            let rect = rectangle(cx, args)?;
            self.update(|r| {
                let left = rect.left.max(0);
                let top = rect.top.max(0);
                let right = (i64::from(rect.left) + i64::from(rect.width))
                    .min(i64::from(r.geometry.image_size.width))
                    .max(i64::from(left));
                let bottom = (i64::from(rect.top) + i64::from(rect.height))
                    .min(i64::from(r.geometry.image_size.height))
                    .max(i64::from(top));
                r.geometry.clip = Rect {
                    left,
                    top,
                    width: (right - i64::from(left)) as u32,
                    height: (bottom - i64::from(top)) as u32,
                };
                Ok(())
            })
        }
        #[tjs::setter(name = "clipLeft")]
        fn set_clipleft(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let clip = self.read(|r| r.geometry.clip)?;
            self.set_clip(
                cx,
                &[
                    input,
                    Value::Int(clip.top as i64),
                    Value::Int(clip.width as i64),
                    Value::Int(clip.height as i64),
                ],
            )
        }
        #[tjs::setter(name = "clipTop")]
        fn set_cliptop(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let clip = self.read(|r| r.geometry.clip)?;
            self.set_clip(
                cx,
                &[
                    Value::Int(clip.left as i64),
                    input,
                    Value::Int(clip.width as i64),
                    Value::Int(clip.height as i64),
                ],
            )
        }
        #[tjs::setter(name = "clipWidth")]
        fn set_clipwidth(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let clip = self.read(|r| r.geometry.clip)?;
            self.set_clip(
                cx,
                &[
                    Value::Int(clip.left as i64),
                    Value::Int(clip.top as i64),
                    input,
                    Value::Int(clip.height as i64),
                ],
            )
        }
        #[tjs::setter(name = "clipHeight")]
        fn set_clipheight(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let clip = self.read(|r| r.geometry.clip)?;
            self.set_clip(
                cx,
                &[
                    Value::Int(clip.left as i64),
                    Value::Int(clip.top as i64),
                    Value::Int(clip.width as i64),
                    input,
                ],
            )
        }
        #[tjs::method(name = "resetClip")]
        fn reset_clip(&self) -> NativeResult<()> {
            self.require_main()?;
            self.update(|r| {
                r.geometry.clip = r.geometry.image_size.rect();
                Ok(())
            })
        }
        #[tjs::method(name = "fillRect", resumable = true)]
        fn fill_rect(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() < 5 {
                return Err(NativeError::Message(
                    "fillRect requires a rectangle and color",
                ));
            }
            self.fill(rectangle(cx, args)?, integer(cx, args[4])? as u32, None)
        }
        #[tjs::method(name = "colorRect", resumable = true)]
        fn color_rect(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.color(cx, args)
        }
        #[tjs::method(name = "operateRect", resumable = true)]
        fn operate_rect(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.operate(cx, args)
        }
        #[tjs::method(name = "pileRect", resumable = true)]
        fn pile_rect(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            self.legacy_blend(cx, args, 7, true)
        }
        #[tjs::method(name = "blendRect", resumable = true)]
        fn blend_rect(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.legacy_blend(cx, args, 7, false)
        }
        #[tjs::method(name = "piledCopy", resumable = true)]
        fn copy_piled(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.piled_copy(cx, args)
        }
        #[tjs::method(name = "copyRect", resumable = true)]
        fn copy_rect(&self, cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() < 7 {
                return Err(NativeError::Message(
                    "copyRect requires destination, source layer and source rectangle",
                ));
            }
            let source_data = self.drawing_source(cx, args[2])?;
            let lease = self.lease()?;
            let (image, source, clip, face, hold_alpha) = {
                let world = lease.shared.borrow();
                let r = world.record(lease.id)?;
                let face = r.face()?;
                if face != DrawFace::Province {
                    r.image()?;
                    source_data.image()?;
                }
                (
                    r.image.clone(),
                    source_data.image.clone(),
                    r.geometry.clip,
                    face,
                    r.hold_alpha,
                )
            };
            let rectangle = rectangle(cx, &args[3..])?;
            let x = integer(cx, args[0])?;
            let y = integer(cx, args[1])?;
            if face == DrawFace::Province
                && let Some(size) = source_data.bitmap_size()
            {
                // A 32-bit Bitmap has no province plane. Copying that absent
                // plane clears only the source-clipped destination rectangle.
                let Some(clipped) = rectangle.intersection(size.rect()) else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                return self.fill(
                    Rect {
                        left: x.wrapping_add(clipped.left.wrapping_sub(rectangle.left)),
                        top: y.wrapping_add(clipped.top.wrapping_sub(rectangle.top)),
                        ..clipped
                    },
                    0,
                    Some(DrawFace::Province),
                );
            }
            let Some(source) = source else {
                return self.fill(
                    Rect {
                        left: x,
                        top: y,
                        ..rectangle
                    },
                    0,
                    Some(DrawFace::Province),
                );
            };
            let Some(image) = image else {
                return self.create_province(krkr_protocol::graphics::ProvinceOperation::Copy {
                    source,
                    rectangle,
                    x,
                    y,
                    clip,
                });
            };
            source_data.command(
                self,
                Command::Copy {
                    image,
                    source,
                    rectangle,
                    x,
                    y,
                    clip,
                    face,
                    hold_alpha,
                },
                Change::None,
            )
        }
        #[tjs::method(name = "stretchCopy", resumable = true)]
        fn stretch_copy(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.stretch(cx, args, false)
        }
        #[tjs::method(name = "operateStretch", resumable = true)]
        fn operate_stretch(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.stretch(cx, args, true)
        }
        #[tjs::method(name = "stretchPile", resumable = true)]
        fn stretch_pile(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.legacy_blend(cx, args, 9, true)
        }
        #[tjs::method(name = "stretchBlend", resumable = true)]
        fn stretch_blend(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.legacy_blend(cx, args, 9, false)
        }
        #[tjs::method(name = "affineCopy", resumable = true)]
        fn affine_copy(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.affine(cx, args, false)
        }
        #[tjs::method(name = "operateAffine", resumable = true)]
        fn operate_affine(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.affine(cx, args, true)
        }
        #[tjs::method(name = "affineBlend", resumable = true)]
        fn affine_blend(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.legacy_blend(cx, args, 12, false)
        }
        #[tjs::method(name = "affinePile", resumable = true)]
        fn affine_pile(
            &self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            self.affine_pile_legacy(cx, args)
        }
        #[tjs::method(name = "getMainPixel", resumable = true)]
        fn get_main_pixel(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
        ) -> NativeResult<NativeStep> {
            self.pixel(cx, x, y, false, 0xffffff, 0)
        }
        #[tjs::method(name = "getMaskPixel", resumable = true)]
        fn get_mask_pixel(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
        ) -> NativeResult<NativeStep> {
            self.pixel(cx, x, y, false, 255, 24)
        }
        #[tjs::method(name = "getProvincePixel", resumable = true)]
        fn get_province_pixel(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
        ) -> NativeResult<NativeStep> {
            self.pixel(cx, x, y, true, 255, 0)
        }
        #[tjs::method(name = "setMainPixel", resumable = true)]
        fn set_main_pixel(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
            color: Value,
        ) -> NativeResult<NativeStep> {
            self.put_pixel(cx, x, y, color, DrawFace::Opaque)
        }
        #[tjs::method(name = "setMaskPixel", resumable = true)]
        fn set_mask_pixel(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
            color: Value,
        ) -> NativeResult<NativeStep> {
            self.put_pixel(cx, x, y, color, DrawFace::Mask)
        }
        #[tjs::method(name = "setProvincePixel", resumable = true)]
        fn set_province_pixel(
            &self,
            cx: &mut NativeCx<'_>,
            x: Value,
            y: Value,
            color: Value,
        ) -> NativeResult<NativeStep> {
            self.put_pixel(cx, x, y, color, DrawFace::Province)
        }
        #[tjs::getter]
        fn parent(&self) -> NativeResult<Value> {
            let lease = self.lease()?;
            let world = lease.shared.borrow();
            Ok(world
                .record(lease.id)?
                .parent
                .and_then(|id| world.records.get(id))
                .map_or(null(), |r| object(r.owner)))
        }
        #[tjs::setter(name = "parent", resumable = true)]
        fn set_parent(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            self.reparent(cx, input)
        }
        #[tjs::getter(name = "order")]
        fn order(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().order(lease.id, false)? as i64)
        }
        #[tjs::setter(name = "order")]
        fn set_order(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let lease = self.lease()?;
            lease
                .shared
                .borrow_mut()
                .set_order(lease.id, integer(cx, input)?, false)
        }
        #[tjs::getter(name = "absoluteOrder")]
        fn absolute_order(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().order(lease.id, true)? as i64)
        }
        #[tjs::setter(name = "absoluteOrder")]
        fn set_absolute_order(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let lease = self.lease()?;
            lease
                .shared
                .borrow_mut()
                .set_order(lease.id, integer(cx, input)?, true)
        }
        #[tjs::getter(name = "absolute")]
        fn absolute(&self) -> NativeResult<i64> {
            self.absolute_order()
        }
        #[tjs::setter(name = "absolute")]
        fn set_absolute(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            self.set_absolute_order(cx, input)
        }
        #[tjs::getter(name = "absoluteOrderMode")]
        fn absolute_order_mode(&self) -> NativeResult<bool> {
            self.read(|r| r.absolute_order_mode)
        }
        #[tjs::setter(name = "absoluteOrderMode")]
        fn set_absolute_order_mode(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let lease = self.lease()?;
            lease
                .shared
                .borrow_mut()
                .set_order_mode(lease.id, input.truthy(cx.heap())?)
        }
        #[tjs::method(name = "moveBefore")]
        fn move_before(&self, cx: &mut NativeCx<'_>, other: Value) -> NativeResult<()> {
            let other = layer_id(cx.heap_mut(), other)?;
            let lease = self.lease()?;
            lease
                .shared
                .borrow_mut()
                .move_sibling(lease.id, other, true)
        }
        #[tjs::method(name = "moveBehind")]
        fn move_behind(&self, cx: &mut NativeCx<'_>, other: Value) -> NativeResult<()> {
            let other = layer_id(cx.heap_mut(), other)?;
            let lease = self.lease()?;
            lease
                .shared
                .borrow_mut()
                .move_sibling(lease.id, other, false)
        }
        #[tjs::method(resumable = true)]
        fn exchange(&self, cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            self.change_input(Operation::Exchange(layer_id(cx.heap_mut(), other)?, false))
        }
        #[tjs::method(resumable = true)]
        fn swap(&self, cx: &mut NativeCx<'_>, other: Value) -> NativeResult<NativeStep> {
            self.change_input(Operation::Exchange(layer_id(cx.heap_mut(), other)?, true))
        }
        #[tjs::method(name = "bringToFront")]
        fn bring_to_front(&self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            self.set_order(cx, Value::Int(i32::MAX as i64))
        }
        #[tjs::method(name = "sendToBack")]
        fn send_to_back(&self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            self.set_order(cx, Value::Int(0))
        }
        #[tjs::getter]
        fn children(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let lease = self.lease()?;
            let mut world = lease.shared.borrow_mut();
            let record = world.record(lease.id)?;
            let array = record
                .children_array
                .unwrap_or_else(|| cx.heap_mut().alloc_array());
            if record.children_dirty {
                let children = record
                    .children
                    .iter()
                    .filter_map(|id| world.records.get(*id))
                    .map(|r| object(r.owner))
                    .collect();
                cx.heap_mut().array_replace(array, children)?;
            }
            let record = world.record_mut(lease.id)?;
            record.children_array = Some(array);
            record.children_dirty = false;
            Ok(object(array))
        }
        #[tjs::getter(name = "isPrimary")]
        fn is_primary(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease.shared.borrow().is_primary(lease.id))
        }
        #[tjs::getter]
        fn name(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let name = if self.lease.is_none() && self.service.is_none() {
                self.pending_name.clone()
            } else {
                self.read(|r| r.name.clone())?
            };
            Ok(Value::Str(cx.heap_mut().alloc_string(name)))
        }
        #[tjs::setter(name = "name")]
        fn set_name(&mut self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let Value::Str(id) = value::to_string(cx.heap_mut(), input)? else {
                unreachable!()
            };
            let name = cx.heap().string(id)?.to_vec();
            if self.lease.is_none() && self.service.is_none() {
                self.pending_name = name;
                return Ok(());
            }
            self.update(|r| {
                r.name = name;
                Ok(())
            })
        }
        #[tjs::getter(name = "type")]
        fn get_type(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.blend as i64)
                })?
        }
        #[tjs::setter(name = "type", resumable = true)]
        fn set_type(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            let blend = Blend::from_legacy(integer(cx, input)?)
                .ok_or(NativeError::Message("not a drawable Layer type"))?;
            self.change_blend(blend)
        }
        #[tjs::getter(name = "left")]
        fn get_left(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    if state.lease.is_none() && state.service.is_none() {
                        return Ok(0);
                    }
                    state.read(|r| r.geometry.left as i64)
                })?
        }
        #[tjs::getter(name = "top")]
        fn get_top(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    if state.lease.is_none() && state.service.is_none() {
                        return Ok(0);
                    }
                    state.read(|r| r.geometry.top as i64)
                })?
        }
        #[tjs::getter(name = "width")]
        fn get_width(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| i64::from(r.geometry.width))
                })?
        }
        #[tjs::getter(name = "height")]
        fn get_height(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| i64::from(r.geometry.height))
                })?
        }
        #[tjs::getter(name = "imageLeft")]
        fn get_imageleft(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.require_main()?;
                    state.read(|r| r.geometry.image_left as i64)
                })?
        }
        #[tjs::getter(name = "imageTop")]
        fn get_imagetop(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.require_main()?;
                    state.read(|r| r.geometry.image_top as i64)
                })?
        }
        #[tjs::getter(name = "imageWidth")]
        fn get_imagewidth(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.require_main()?;
                    state.read(|r| r.geometry.image_size.width as i64)
                })?
        }
        #[tjs::getter(name = "imageHeight")]
        fn get_imageheight(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.require_main()?;
                    state.read(|r| r.geometry.image_size.height as i64)
                })?
        }
        #[tjs::getter(name = "clipLeft")]
        fn get_clipleft(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.geometry.clip.left as i64)
                })?
        }
        #[tjs::getter(name = "clipTop")]
        fn get_cliptop(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.geometry.clip.top as i64)
                })?
        }
        #[tjs::getter(name = "clipWidth")]
        fn get_clipwidth(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.geometry.clip.width as i64)
                })?
        }
        #[tjs::getter(name = "clipHeight")]
        fn get_clipheight(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.geometry.clip.height as i64)
                })?
        }
        #[tjs::getter(name = "opacity")]
        fn get_opacity(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| {
                    state.read(|r| r.opacity as i64)
                })?
        }
        #[tjs::getter(name = "face")]
        fn get_face(cx: &NativeCx<'_>) -> NativeResult<i64> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| state.read(|r| r.face as i64))?
        }
        #[tjs::getter(name = "visible")]
        fn get_visible(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| state.read(|r| r.visible))?
        }
        #[tjs::setter(name = "visible", resumable = true)]
        fn set_visible(&self, value: bool) -> NativeResult<NativeStep> {
            self.change_input(Operation::Visible(value))
        }
        #[tjs::getter(name = "holdAlpha")]
        fn get_hold_alpha(cx: &NativeCx<'_>) -> NativeResult<bool> {
            cx.heap()
                .inspect_native_state::<Self, _>(cx.this(), |state| state.read(|r| r.hold_alpha))?
        }
        #[tjs::setter(name = "holdAlpha")]
        fn set_hold_alpha(&self, input: bool) -> NativeResult<()> {
            self.update(|r| {
                r.hold_alpha = input;
                Ok(())
            })
        }
        #[tjs::setter(name = "width", resumable = true)]
        fn set_width(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            let input = integer(cx, input)?;
            let other = self.read(|r| r.geometry.height)?;
            self.change_size(input, other, false)
        }
        #[tjs::setter(name = "height", resumable = true)]
        fn set_height(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            let input = integer(cx, input)?;
            let other = self.read(|r| r.geometry.width)?;
            self.change_size(other, input, false)
        }
        #[tjs::setter(name = "imageWidth", resumable = true)]
        fn set_imagewidth(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            let input = integer(cx, input)?;
            let other = self.read(|r| r.geometry.image_size.height as i32)?;
            self.change_size(input, other, true)
        }
        #[tjs::setter(name = "imageHeight", resumable = true)]
        fn set_imageheight(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
            let input = integer(cx, input)?;
            let other = self.read(|r| r.geometry.image_size.width as i32)?;
            self.change_size(other, input, true)
        }
        #[tjs::setter(name = "left")]
        fn set_left(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let input = integer(cx, input)?;
            // KAG subclasses initialize left/top before calling Layer's base
            // constructor. Setting the native default is already a no-op.
            if input == 0 && self.lease.is_none() && self.service.is_none() {
                return Ok(());
            }
            let other = self.read(|r| r.geometry.top)?;
            self.move_to(input, other)
        }
        #[tjs::setter(name = "top")]
        fn set_top(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let input = integer(cx, input)?;
            if input == 0 && self.lease.is_none() && self.service.is_none() {
                return Ok(());
            }
            let other = self.read(|r| r.geometry.left)?;
            self.move_to(other, input)
        }
        #[tjs::setter(name = "imageLeft")]
        fn set_imageleft(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let other = self.read(|r| r.geometry.image_top as i64)?;
            self.set_image_pos(cx, input, Value::Int(other))
        }
        #[tjs::setter(name = "imageTop")]
        fn set_imagetop(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let other = self.read(|r| r.geometry.image_left as i64)?;
            self.set_image_pos(cx, Value::Int(other), input)
        }
        #[tjs::setter(name = "opacity")]
        fn set_opacity(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let input = integer(cx, input)?.clamp(0, 255) as u8;
            let primary = self.is_primary()?;
            self.update_if_changed(|r| {
                if primary && input != 255 {
                    return Err(NativeError::Message("primary layer must remain opaque"));
                }
                if r.opacity == input {
                    return Ok(false);
                }
                r.opacity = input;
                Ok(true)
            })
        }
        #[tjs::setter(name = "face")]
        fn set_face(&self, cx: &mut NativeCx<'_>, input: Value) -> NativeResult<()> {
            let input = integer(cx, input)?;
            if !matches!(input, 0..=4 | 128) {
                return Err(NativeError::Message("invalid draw face"));
            }
            self.update(|r| {
                r.face = input;
                Ok(())
            })
        }
    }
}
pub(super) fn install(heap: &mut Heap, shared: Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<State>(class)?;
    heap.with_native_state::<State, _>(class, |s| s.service = Some(shared))?;
    Ok(())
}
