use super::*;
use tjs_core::NativeStep;

#[tjs_bind::class(name = "KAGParser")]
pub(super) mod implementation {
    use super::*;
    pub struct State {
        pub parser: Parser,
        pub tag: Option<ObjId>,
        pub macros: Option<ObjId>,
        pub extended: bool,
        pub advanced: Option<advanced::Advanced>,
        pub param_macros: Option<ObjId>,
        pub tag_order: Option<ObjId>,
        pub arg_order: Vec<ObjId>,
        pub args: Vec<ObjId>,
        pub cache: Rc<RefCell<Cache>>,
        pub operations: Option<crate::operations::Shared>,
        pub keys: Option<Keys>,
        pub characters: HashMap<u16, Value>,
        pub debug_level: i64,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                parser: Parser::default(),
                tag: None,
                macros: None,
                extended: false,
                advanced: None,
                param_macros: None,
                tag_order: None,
                arg_order: Vec::new(),
                args: Vec::new(),
                cache: Rc::default(),
                operations: None,
                keys: None,
                characters: HashMap::new(),
                debug_level: 1,
            }
        }
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.advanced.trace(visit);
            for id in self
                .tag
                .into_iter()
                .chain(self.macros)
                .chain(self.param_macros)
                .chain(self.tag_order)
                .chain(self.arg_order.iter().copied())
                .chain(self.args.iter().copied())
            {
                visit(Value::Obj(id.into()));
            }
            if let Some(keys) = &self.keys {
                keys.trace(visit);
            }
            for &value in self.characters.values() {
                visit(value);
            }
        }
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>) -> NativeResult<Self> {
            let extended = cx.with_state::<State, _>(|s, _| Ok(s.extended))?;
            let advanced = cx.with_state::<State, _>(|s, _| Ok(s.advanced.is_some()))?;
            let class = cx.heap().registered_class("KAGParser").unwrap();
            let (cache, operations) = cx.heap_mut().with_native_state::<State, _>(class, |s| {
                (s.cache.clone(), s.operations.clone())
            })?;
            let mut parser = Parser::default();
            parser.advanced = advanced;
            Ok(Self {
                parser,
                advanced: if advanced {
                    Some(advanced::Advanced::new(cx.heap_mut())?)
                } else {
                    None
                },
                cache,
                operations,
                tag: Some(cx.heap_mut().alloc_dictionary()),
                macros: Some(cx.heap_mut().alloc_dictionary()),
                extended,
                param_macros: extended.then(|| cx.heap_mut().alloc_dictionary()),
                tag_order: extended.then(|| cx.heap_mut().alloc_array()),
                keys: Some(Keys::new(cx.heap_mut())?),
                ..Self::default()
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(name = "getNextTag", resumable = true)]
        fn next(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            task::Task::next(cx)
        }
        #[tjs::method(name = "loadScenario", resumable = true)]
        fn load(cx: &mut NativeCx<'_>, name: Value) -> NativeResult<NativeStep> {
            let name = text(cx, name)?;
            task::Task::load(cx, name)
        }
        #[tjs::method(name = "goToLabel", resumable = true)]
        fn go(cx: &mut NativeCx<'_>, label: Value) -> NativeResult<NativeStep> {
            let label = text(cx, label)?;
            task::Task::go(cx, label, false)
        }
        #[tjs::method(name = "callLabel", resumable = true)]
        fn call(cx: &mut NativeCx<'_>, label: Value) -> NativeResult<NativeStep> {
            let label = text(cx, label)?;
            task::Task::go(cx, label, true)
        }
        #[tjs::method]
        fn clear(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            self.parser.clear();
            self.args.clear();
            self.arg_order.clear();
            *self.cache.borrow_mut() = Cache::default();
            if let Some(advanced) = self.advanced.as_mut() {
                advanced.clear(cx.heap_mut())?;
            }
            Ok(())
        }
        #[tjs::method(name = "clearCallStack")]
        fn clear_calls(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
            self.parser.clear_calls();
            if let Some(advanced) = self.advanced.as_mut() {
                advanced.clear(cx.heap_mut())?;
            }
            Ok(())
        }
        #[tjs::method(name = "popMacroArgs")]
        fn pop(&mut self) -> NativeResult<()> {
            self.parser.pop_macro().map_err(error)
        }
        #[tjs::method]
        fn interrupt(&mut self) {
            self.parser.interrupted = true;
        }
        #[tjs::method(name = "resetInterrupt")]
        fn reset_interrupt(&mut self) {
            self.parser.interrupted = false;
        }
        #[tjs::method]
        fn assign(cx: &mut NativeCx<'_>, source: Value) -> NativeResult<()> {
            save::assign(cx, object(source)?)
        }
        #[tjs::method]
        fn store(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            save::store(cx, self)
        }
        #[tjs::method(resumable = true)]
        fn restore(cx: &mut NativeCx<'_>, data: Value) -> NativeResult<NativeStep> {
            save::restore(cx, object(data)?)
        }
        #[tjs::getter(name = "curLine")]
        fn line(&self) -> i64 {
            self.parser.position.line as i64
        }
        #[tjs::getter(name = "curPos")]
        fn position(&self) -> i64 {
            self.parser.position.pos as i64
        }
        #[tjs::getter(name = "curLineStr")]
        fn line_string(&self, cx: &mut NativeCx<'_>) -> Value {
            string(cx.heap_mut(), self.parser.line().to_vec())
        }
        #[tjs::getter(name = "curStorage")]
        fn storage(&self, cx: &mut NativeCx<'_>) -> Value {
            string(cx.heap_mut(), self.parser.storage.clone())
        }
        #[tjs::setter(name = "curStorage", resumable = true)]
        fn set_storage(cx: &mut NativeCx<'_>, name: Value) -> NativeResult<NativeStep> {
            let name = text(cx, name)?;
            task::Task::load(cx, name)
        }
        #[tjs::getter(name = "curLabel")]
        fn label(&self, cx: &mut NativeCx<'_>) -> Value {
            string(cx.heap_mut(), self.parser.label.clone())
        }
        #[tjs::getter(name = "processSpecialTags")]
        fn special(&self) -> bool {
            self.parser.process_special
        }
        #[tjs::setter(name = "processSpecialTags")]
        fn set_special(&mut self, value: bool) {
            self.parser.process_special = value;
        }
        #[tjs::getter(name = "ignoreCR")]
        fn ignore_cr(&self) -> bool {
            self.parser.ignore_cr
        }
        #[tjs::setter(name = "ignoreCR")]
        fn set_ignore_cr(&mut self, value: bool) {
            self.parser.ignore_cr = value;
        }
        #[tjs::getter(name = "debugLevel")]
        fn debug(&self) -> i64 {
            self.debug_level
        }
        #[tjs::setter(name = "debugLevel")]
        fn set_debug(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            self.debug_level = value::to_integer(cx.heap(), value)?;
            Ok(())
        }
        #[tjs::getter]
        fn macros(&self) -> NativeResult<Value> {
            Ok(Value::Obj(ObjRef::bound(self.macros_id()?)))
        }
        #[tjs::getter(name = "macroParams")]
        fn params(&self) -> NativeResult<Value> {
            if self.advanced.is_some() && self.parser.macro_depth == 0 {
                return Err(NativeError::Message("macro arguments outside macro"));
            }
            Ok(self
                .params_id()
                .map_or(Value::Obj(ObjRef::default()), |id| {
                    Value::Obj(ObjRef::bound(id))
                }))
        }
        #[tjs::getter]
        fn mp(&self) -> NativeResult<Value> {
            if self.advanced.is_some() && self.parser.macro_depth == 0 {
                return Err(NativeError::Message("macro arguments outside macro"));
            }
            Ok(self
                .params_id()
                .map_or(Value::Obj(ObjRef::default()), |id| {
                    Value::Obj(ObjRef::bound(id))
                }))
        }
        #[tjs::getter(name = "callStackDepth")]
        fn depth(&self) -> i64 {
            self.parser.calls.len() as i64
        }
    }
}
