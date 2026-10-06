#[path = "support/events.rs"]
mod support;
use krkr_engine::plugins::{self, Context, Plugin};
use std::{cell::Cell, rc::Rc};
use support::*;
use tjs_core::{NativeError, NativeResult, ObjRef, Trace, Value};
use tjs_runtime::Runtime;

#[tjs_bind::class(name = "PluginCounter")]
mod counter {
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        value: i64,
    }
    impl State {
        #[tjs::constructor]
        fn new(value: i64) -> Self {
            Self { value }
        }
        #[tjs::method]
        fn increment(&mut self) -> i64 {
            self.value += 1;
            self.value
        }
    }
}
struct CounterPlugin {
    class: Value,
    can_unload: Rc<Cell<bool>>,
    fail_link: Rc<Cell<bool>>,
    links: Rc<Cell<u32>>,
}
impl Trace for CounterPlugin {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(self.class);
    }
}
impl Plugin for CounterPlugin {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        self.class = Value::Obj(ObjRef::bound(counter::install(cx.heap)?));
        cx.export("PluginCounter", self.class)?;
        if self.fail_link.replace(false) {
            return Err(NativeError::Message("link failed"));
        }
        self.links.set(self.links.get() + 1);
        Ok(())
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        cx.remove("PluginCounter")?;
        if !self.can_unload.get() {
            return Ok(false);
        }
        self.class = Value::Void;
        Ok(true)
    }
}

#[test]
fn registered_native_plugin_preserves_names_aliases_failure_and_unload_contracts() {
    let mut runtime = Runtime::new();
    let can_unload = Rc::new(Cell::new(false));
    let fail_link = Rc::new(Cell::new(true));
    let links = Rc::new(Cell::new(0));
    plugins::register(
        &mut runtime.heap,
        &["counter.dll", "counter.tpm"],
        CounterPlugin {
            class: Value::Void,
            can_unload: can_unload.clone(),
            fail_link,
            links: links.clone(),
        },
    )
    .unwrap();
    let (mut engine, _) = setup(runtime, Default::default());
    assert_eq!(
        run(
            &mut engine,
            r#"
        var errors=0;
        try{Plugins.link('missing.dll');}catch(e){errors++;}
        try{Plugins.link('Counter.dll');}catch(e){errors++;}
        errors==2 && Plugins.getList().count==0 && !('PluginCounter' in global);
    "#
        ),
        "1"
    );
    // Failed native initialization remains tracked when its cleanup refuses.
    // Retrying must not publish that partial provider or count it as loaded.
    assert_eq!(
        run(
            &mut engine,
            "var retryError='';try{Plugins.link('counter.tpm');}catch(e){retryError=e.message;} retryError=='failed native plugin must be unlinked before retry' && !Plugins.unlink('Counter.dll') && Plugins.getList().count==0 && !('PluginCounter' in global);"
        ),
        "1"
    );
    assert_eq!(links.get(), 0);
    can_unload.set(true);
    assert_eq!(run(&mut engine, "Plugins.unlink('counter.tpm');"), "1");
    can_unload.set(false);
    assert_eq!(
        run(
            &mut engine,
            r#"
        Plugins.link('Counter.dll'); Plugins.link('Counter.dll'); Plugins.link('counter.tpm');
        var c=new PluginCounter(40), first=c.increment();
        var list=Plugins.getList(); list.clear();
        first==41 && Plugins.getList().join(',')=='Counter.dll,counter.tpm';
    "#
        ),
        "1"
    );
    assert_eq!(links.get(), 1);
    assert_eq!(
        run(
            &mut engine,
            r#"
        var removed=Plugins.unlink('Counter.dll');
        removed && !Plugins.unlink('counter.tpm') && c.increment()==42 && Plugins.getList().count==1;
    "#
        ),
        "1"
    );
    can_unload.set(true);
    assert_eq!(
        run(
            &mut engine,
            r#"
        invalidate c; var retainedClass=PluginCounter;
        var removed=Plugins.unlink('counter.tpm'), errors=0;
        try{Plugins.unlink('Counter.dll');}catch(e){errors++;}
        removed && errors==1 && Plugins.getList().count==0 && !('PluginCounter' in global)
            && (new retainedClass(10)).increment()==11;
    "#
        ),
        "1"
    );
    assert_eq!(
        run(
            &mut engine,
            "Plugins.link('Counter.dll'); (new PluginCounter(0)).increment();"
        ),
        "1"
    );
    assert_eq!(links.get(), 2);
}

#[test]
fn native_plugin_paths_preserve_identity_and_explicit_path_overrides() {
    let mut runtime = Runtime::new();
    let links = Rc::new(Cell::new(0));
    plugins::register(
        &mut runtime.heap,
        &["counter.dll", "counter.tpm"],
        CounterPlugin {
            class: Value::Void,
            can_unload: Rc::new(Cell::new(true)),
            fail_link: Rc::new(Cell::new(false)),
            links: links.clone(),
        },
    )
    .unwrap();
    let (mut engine, _) = setup(runtime, Default::default());
    assert_eq!(
        run(
            &mut engine,
            r#"
        Plugins.link('plugin/Counter.dll');
        Plugins.link('C:\\game\\plugin\\counter.tpm');
        if(!Plugins.unlink('plugin/Counter.dll')) throw 'alias release';
        var n=(new PluginCounter(6)).increment();
        if(!Plugins.unlink('C:\\game\\plugin\\counter.tpm')) throw 'last release';
        if('PluginCounter' in global) throw 'exports retained';
        Plugins.mock('private/counter.dll');
        Plugins.link('private/counter.dll');
        if('PluginCounter' in global) throw 'exact path override ignored';
        Plugins.unlink('private/counter.dll');
        Plugins.mock('other.dll');
        var rejected=0;
        try { Plugins.link('plugin/other.dll'); } catch(e) { rejected++; }
        try { Plugins.link('plugin/missing.dll'); } catch(e) { rejected++; }
        n==7 && rejected==2 && Plugins.getList().count==0;
    "#
        ),
        "1"
    );
    assert_eq!(links.get(), 1);
}

struct Edits {
    outcome: Rc<Cell<u8>>,
    traces: Rc<Cell<usize>>,
}
impl Trace for Edits {
    fn trace(&self, _: &mut dyn FnMut(Value)) {
        self.traces.set(self.traces.get() + 1);
    }
}
impl Plugin for Edits {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        cx.export("existing", Value::Int(8))?;
        cx.remove("kept")?;
        cx.export("transient", Value::Int(1))?;
        cx.remove("transient")?;
        assert!(matches!(cx.exported("existing")?, Some(Value::Int(8))));
        assert!(cx.exported("transient")?.is_none());
        if self.outcome.get() == 0 {
            return Err(NativeError::Message("failure after pending writes"));
        }
        // A host hook cannot mutate the registry underneath an active hook.
        assert!(
            plugins::register(
                cx.heap,
                &["nested"],
                Edits {
                    outcome: self.outcome.clone(),
                    traces: self.traces.clone(),
                }
            )
            .is_err()
        );
        Ok(())
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        cx.remove("existing")?;
        cx.export("kept", Value::Int(12))?;
        match self.outcome.get() {
            0 => Err(NativeError::Message("unload failed after pending writes")),
            1 => Ok(false),
            _ => Ok(true),
        }
    }
}

#[test]
fn pending_exports_preserve_overwritten_members_on_failure_and_unload_refusal() {
    let mut runtime = Runtime::new();
    let outcome = Rc::new(Cell::new(0));
    plugins::register(
        &mut runtime.heap,
        &["edits"],
        Edits {
            outcome: outcome.clone(),
            traces: Rc::new(Cell::new(0)),
        },
    )
    .unwrap();
    let (mut engine, _) = setup(runtime, Default::default());
    let global = engine.global();
    let existing = engine
        .runtime_mut()
        .heap
        .intern(&"existing".encode_utf16().collect::<Vec<_>>());
    engine
        .runtime_mut()
        .heap
        .set_member_flags(global, existing, Value::Int(7), true, true)
        .unwrap();
    assert_eq!(
        run(
            &mut engine,
            "var kept=9;try{Plugins.link('edits');}catch(e){} existing==7 && kept==9 && !('transient' in global) && Plugins.getList().count==0;"
        ),
        "1"
    );
    assert!(
        !engine
            .runtime()
            .heap
            .members(global)
            .unwrap()
            .any(|(name, _)| name == existing)
    );
    // Cleanup also failed, so the provider must be explicitly released before
    // another link. Failed cleanup must not publish its staged export changes.
    assert_eq!(
        run(
            &mut engine,
            "var retryError='';try{Plugins.link('edits');}catch(e){retryError=e.message;} retryError=='failed native plugin must be unlinked before retry' && existing==7 && kept==9 && !('transient' in global) && Plugins.getList().count==0;"
        ),
        "1"
    );
    outcome.set(2);
    assert_eq!(run(&mut engine, "Plugins.unlink('edits');"), "1");
    outcome.set(1);
    assert_eq!(
        run(
            &mut engine,
            "Plugins.link('edits');existing==8 && !('kept' in global) && !('transient' in global) && !Plugins.unlink('edits') && existing==8 && Plugins.getList().count==1;"
        ),
        "1"
    );
    outcome.set(0);
    assert_eq!(
        run(
            &mut engine,
            "var failed=false;try{Plugins.unlink('edits');}catch(e){failed=true;} failed && existing==8 && !('kept' in global) && Plugins.getList().count==1;"
        ),
        "1"
    );
    outcome.set(2);
    assert_eq!(
        run(
            &mut engine,
            "Plugins.unlink('edits') && !('existing' in global) && kept==12 && Plugins.getList().count==0;"
        ),
        "1"
    );
}

#[test]
fn alias_validation_is_atomic_and_gc_visits_each_provider_once() {
    let mut runtime = Runtime::new();
    let traces = Rc::new(Cell::new(0));
    let plugin = || Edits {
        outcome: Rc::new(Cell::new(2)),
        traces: traces.clone(),
    };
    for aliases in [
        &[][..],
        &["valid", "VALID"],
        &["valid", "bad\0alias"],
        &["valid", ""],
    ] {
        assert!(plugins::register(&mut runtime.heap, aliases, plugin()).is_err());
    }
    plugins::register(&mut runtime.heap, &["valid", "alias"], plugin()).unwrap();
    assert!(plugins::register(&mut runtime.heap, &["free", "VALID"], plugin()).is_err());
    plugins::register(&mut runtime.heap, &["free"], plugin()).unwrap();
    runtime.collect([]);
    assert_eq!(traces.get(), 2);
}
