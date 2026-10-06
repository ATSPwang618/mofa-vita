use super::*;
#[tjs_bind::class(name = "WaveFlags")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub link: Option<(Shared, SoundId, ObjId)>,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            if let Some((_, _, owner)) = &self.link {
                owner.trace(visit);
            }
        }
    }
    impl State {
        fn buffer(&self) -> NativeResult<Option<krkr_audio::Handle>> {
            let (w, id, _) = self.link.as_ref().ok_or(NativeError::This)?;
            Ok(w.borrow().record(*id)?.handle.clone())
        }
        #[tjs::constructor]
        fn create(cx: &mut NativeCx<'_>, buffer: Value) -> NativeResult<Self> {
            Ok(Self {
                link: Some(bindings::link(cx.heap_mut(), buffer)?),
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method]
        fn reset(&self) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                for i in 0..16 {
                    h.set_flag(i, 0);
                }
            }
            Ok(())
        }
        #[tjs::getter(name = "count")]
        fn count(&self) -> i64 {
            16
        }

        #[tjs::getter(name = "0")]
        fn get_0(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(0) as i64))
        }
        #[tjs::setter(name = "0")]
        fn set_0(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(0, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "1")]
        fn get_1(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(1) as i64))
        }
        #[tjs::setter(name = "1")]
        fn set_1(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(1, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "2")]
        fn get_2(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(2) as i64))
        }
        #[tjs::setter(name = "2")]
        fn set_2(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(2, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "3")]
        fn get_3(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(3) as i64))
        }
        #[tjs::setter(name = "3")]
        fn set_3(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(3, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "4")]
        fn get_4(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(4) as i64))
        }
        #[tjs::setter(name = "4")]
        fn set_4(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(4, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "5")]
        fn get_5(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(5) as i64))
        }
        #[tjs::setter(name = "5")]
        fn set_5(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(5, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "6")]
        fn get_6(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(6) as i64))
        }
        #[tjs::setter(name = "6")]
        fn set_6(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(6, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "7")]
        fn get_7(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(7) as i64))
        }
        #[tjs::setter(name = "7")]
        fn set_7(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(7, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "8")]
        fn get_8(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(8) as i64))
        }
        #[tjs::setter(name = "8")]
        fn set_8(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(8, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "9")]
        fn get_9(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(9) as i64))
        }
        #[tjs::setter(name = "9")]
        fn set_9(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(9, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "10")]
        fn get_10(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(10) as i64))
        }
        #[tjs::setter(name = "10")]
        fn set_10(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(10, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "11")]
        fn get_11(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(11) as i64))
        }
        #[tjs::setter(name = "11")]
        fn set_11(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(11, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "12")]
        fn get_12(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(12) as i64))
        }
        #[tjs::setter(name = "12")]
        fn set_12(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(12, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "13")]
        fn get_13(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(13) as i64))
        }
        #[tjs::setter(name = "13")]
        fn set_13(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(13, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "14")]
        fn get_14(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(14) as i64))
        }
        #[tjs::setter(name = "14")]
        fn set_14(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(14, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }

        #[tjs::getter(name = "15")]
        fn get_15(&self) -> NativeResult<i64> {
            Ok(self.buffer()?.map_or(0, |h| h.flag(15) as i64))
        }
        #[tjs::setter(name = "15")]
        fn set_15(&mut self, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<()> {
            if let Some(h) = self.buffer()? {
                h.set_flag(15, tjs_core::value::to_integer(cx.heap(), v)? as i32);
            }
            Ok(())
        }
    }
}
pub(super) fn object(
    heap: &mut Heap,
    shared: Shared,
    id: SoundId,
    owner: ObjId,
) -> NativeResult<ObjId> {
    let class = heap.registered_class("WaveFlags").expect("installed flags");
    heap.alloc_native(
        class,
        implementation::State {
            link: Some((shared, id, owner)),
        },
    )
}
pub(super) fn install(heap: &mut Heap) -> NativeResult<()> {
    implementation::install(heap)?;
    Ok(())
}
