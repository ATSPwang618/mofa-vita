#[path = "../../krkr-assets/tests/support/mod.rs"]
mod support;

use krkr_engine::{Engine, EngineEvent, assets::Vfs};
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{ContextId, Runtime, RuntimeExit, SchedulerLimits, clock::MonotonicClock};

fn setup(vfs: Option<Vfs>) -> Engine<MonotonicClock> {
    let mut runtime = Runtime::new();
    krkr_engine::kag::install(&mut runtime.heap).unwrap();
    if let Some(vfs) = vfs {
        krkr_engine::storages::install(&mut runtime.heap, vfs).unwrap();
    }
    Engine::new(
        runtime,
        MonotonicClock::default(),
        SchedulerLimits {
            max_contexts: 1,
            ..Default::default()
        },
        Default::default(),
    )
    .unwrap()
}
fn submit(engine: &mut Engine<MonotonicClock>, source: &str) -> ContextId {
    let id = engine
        .runtime_mut()
        .sources
        .add_utf8("KAG integration", source)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, id).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"))
}
fn step(engine: &mut Engine<MonotonicClock>) -> EngineEvent {
    let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
    engine.collect([]);
    event
}
fn run(engine: &mut Engine<MonotonicClock>, source: &str) -> String {
    let id = submit(engine, source);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match step(engine) {
            EngineEvent::Completed { context, result } if context == id => {
                let RuntimeExit::Finished(value) = result else {
                    panic!("{result:?}");
                };
                let text = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return text;
            }
            EngineEvent::Waiting { request, .. } => {
                assert!(engine.owns_wait(request));
                std::thread::park_timeout(Duration::from_millis(1));
            }
            EngineEvent::Yielded => {}
            event => panic!("unexpected {event:?}"),
        }
    }
    panic!("scenario did not finish");
}

const VIRTUAL: &str = r#"
class Scenario extends KAGParser {
    function Scenario(source) { super.KAGParser(); this.source=source; debugLevel=0; }
    function onScenarioLoad(name) { return source; }
    function onScript(source,name,line) { Scripts.exec(source,name,line,this); }
}
function parser(source) { var p=new Scenario(source); p.loadScenario('virtual.ks'); return p; }
"#;
fn check(source: &str, expected: &str) {
    let mut engine = setup(None);
    assert_eq!(run(&mut engine, &format!("{VIRTUAL}\n{source}")), expected);
}

#[test]
fn tokens_keep_utf16_units_dictionary_identity_and_original_attribute_rules() {
    check(
        r#"
        var p=parser('*one|page\n[CuStOm Flag X="hello` world" Eval=&"6*7" Esc="`&literal"][[😀[p]\n@LAST\n');
        var labels=''; p.onLabel=function(name,page){ labels=name+':'+page; };
        var a=p.getNextTag();
        var ok=a.tagname=='custom' && a.flag==='true' && a.x=='hello world' && a.eval==='42' && a.esc=='&literal';
        var b=p.getNextTag(); ok=ok && a===b && b.tagname=='ch' && b.text=='[' && b.flag===void;
        var hi=#p.getNextTag().text, lo=#p.getNextTag().text;
        ok=ok && hi==0xd83d && lo==0xde00 && p.getNextTag().tagname=='p' && p.getNextTag().tagname=='last';
        ok && p.getNextTag()===void && labels=='*one:page' && p.mp===null;
    "#,
        "1",
    );
}

#[test]
fn conditions_macros_parameters_and_embedded_script_share_the_vm() {
    check(
        r#"
        global.effects=0; global.answer=40;
        var p=parser('@macro name=outer\n[inner *]\\\n@endmacro\n@macro name=inner\n[custom lost=yes * text=%text missing=%missing|fallback absent=%none]\\\n@endmacro\n@if exp="false"\n[bad x=&"global.effects++"][if exp="global.effects++"][endif]\n@elsif exp="true"\n[outer text=hi]\n@else\nbad\n@endif\n[no cond="false"][yes x=&"void"]\n@iscript\nglobal.answer += 2;\n@endscript\n[emb exp="\'[literal]\'"][done]');
        var tag=p.getNextTag();
        var ok=tag.tagname=='custom' && tag.text=='hi' && tag.missing=='fallback' && tag.absent===void && tag.lost===void && p.mp.text=='hi';
        while ((tag=p.getNextTag())!==void && tag.tagname!='yes') {}
        ok=ok && tag.x===void && p.mp===null && global.effects==0;
        var output='';
        while ((tag=p.getNextTag())!==void) if(tag.tagname=='ch') output+=tag.text;
        ok && output=='[literal]' && global.answer==42;
    "#,
        "1",
    );
}

#[test]
fn call_return_restores_expanded_line_arguments_and_callbacks_can_veto() {
    check(
        r#"
        var p=parser('*main\n[go text=kept][done]\n*sub\n[item text=&"mp.text"][return]\n');
        p.macros.go='[call target=*sub][after text=%text][macropop]';
        var events='';
        p.onCall=function(t){events+='c'; return true;};
        p.onReturn=function(t){events+='r'; return true;};
        p.onAfterReturn=function(){events+='a';};
        var ok=p.getNextTag().text=='kept' && p.callStackDepth==1;
        var next=p.getNextTag(); ok=ok && next.tagname=='after' && next.text=='kept' && p.callStackDepth==0;
        ok=ok && p.getNextTag().tagname=='done' && p.mp===null && events=='cra';
        p=parser('[jump target=*absent][call target=*absent][return][done]');
        p.onJump=p.onCall=p.onReturn=function(t){return false;};
        ok && p.getNextTag().tagname=='done';
    "#,
        "1",
    );
}

#[test]
fn snapshots_restore_labels_assign_copies_cursor_and_load_hooks_see_saved_arguments() {
    check(
        r#"
        var p=parser('*start\n[first][second]');
        p.getNextTag(); var saved=p.store(); var q=new Scenario('unused'); q.assign(p);
        var ok=q.getNextTag().tagname=='second' && p.getNextTag().tagname=='second';
        p.restore(saved); ok=ok && p.getNextTag().tagname=='first';
        p=parser('*start\n[m text=kept][done]'); p.macros.m='[mark][macropop]';
        p.getNextTag(); saved=p.store(); q=new Scenario(p.source);
        q.onScenarioLoad=function(name){ global.visible=this.mp.text+':'+this.store().macroArgs.count; return source; };
        q.restore(saved); ok=ok && visible=='kept:1' && q.mp.text=='kept' && q.macros.m==p.macros.m;
        p.macros.m='changed'; ok && q.macros.m!='changed';
    "#,
        "1",
    );
}

#[test]
fn xp3_loads_and_chained_loads_work_with_one_operation_slot_and_property_assignment() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.ks"),
        b"*main\n@call storage=data.xp3>sub.ks target=*sub\n[done]",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("data.xp3"),
        support::archive(
            "sub.ks",
            b"*sub\n@jump storage=end.ks target=*end",
            true,
            true,
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("end.ks"),
        b"*end\n[item text=loaded][return]",
    )
    .unwrap();
    let mut engine = setup(Some(
        Vfs::new(directory.path(), Default::default()).unwrap(),
    ));
    assert_eq!(
        run(
            &mut engine,
            r#"
        var p=new KAGParser; p.debugLevel=0;
        function load(p) { var prop=&p.curStorage; *prop='main.ks'; } load(p);
        var ok=p.curStorage=='main.ks' && p.getNextTag().text=='loaded' && p.getNextTag().tagname=='done';
        var caught=0; try {p.curStorage='missing.ks';} catch(e){caught++;}
        ok && caught==1;
    "#
        ),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
}

#[test]
fn malformed_scenarios_throw_and_long_native_scans_remain_cancellable() {
    check(
        r#"
        var caught=0;
        var sources=['[bad x="unfinished]', '@iscript\n42;', '[if]', '[return]', '[macropop]'];
        for(var i=0;i<sources.count;i++) {
            try { parser(sources[i]).getNextTag(); } catch(e) { caught++; }
        }
        caught;
    "#,
        "5",
    );
    let mut engine = setup(None);
    run(
        &mut engine,
        &format!("{VIRTUAL}\nvar p=parser('*loop\\n@jump target=*loop');"),
    );
    let id = submit(&mut engine, "p.getNextTag();");
    for _ in 0..200 {
        let event = step(&mut engine);
        assert!(matches!(event, EngineEvent::Yielded), "{event:?}");
    }
    assert_eq!(engine.cancel(id).len(), 1);
    assert_eq!(run(&mut engine, "p.clear(); 42;"), "42");
}
