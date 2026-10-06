use krkr_engine::{Engine, EngineEvent};
use std::{num::NonZeroUsize, time::Duration};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};

struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}
fn arm_missing(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Value::Obj(reference) = args[0] else {
        return Err(NativeError::Type("object"));
    };
    cx.heap_mut().set_call_missing(reference.object.unwrap())?;
    Ok(Value::Void)
}
fn discarded(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    if cx.result_needed() {
        return Err(NativeError::Message(
            "timeline requested a discarded draw result",
        ));
    }
    Ok(Value::Void)
}

#[test]
fn timeline_group_source_contracts_survive_gc() {
    let mut runtime = Runtime::new();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let global = engine.global();
    for (name, call) in [
        ("armMissing", arm_missing as tjs_core::NativeThunk),
        ("discarded", discarded),
    ] {
        let heap = &mut engine.runtime_mut().heap;
        let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
        let function = heap.alloc_native_function(NativeCallable::Leaf(call));
        heap.set_member(global, key, Value::Obj(function.into()))
            .unwrap();
    }
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("ksupport timeline group", SOURCE)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let value = loop {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        match event {
            EngineEvent::Yielded => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(value),
                ..
            } => break value,
            other => panic!("timeline fixture failed: {other:?}"),
        }
    };
    assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
}

// Source-derived from kwidgets e7bed32 timeline.cpp, ncbind.hpp and the original
// KTimeLineControl.tjs caller. C++ argument evaluation order is unspecified;
// selection uses source order while preserving every HasValue/GetValue read.
// This does not claim an original DLL or graphical-host differential run.
const SOURCE: &str = r#"
function check(value, name) { if (!value) throw name; }
function eq(value, expected, name) { if (value !== expected) throw name + ": " + value; }
Plugins.link("ksupport.dll");
var visits=[], calls=[], times=[0,4,10], reads=0;
class FrameTime {
    var t;
    function FrameTime(value) { t=value; }
    property time { getter { global.visits.add("t"+t); return t; } }
}
class FrameList {
    property count { getter { global.visits.add("count"); return global.times.count; } }
    function missing(set,name,value) {
        global.visits.add(name);
        var i=int name;
        if (i>=0 && i<global.times.count) { *value=new FrameTime(global.times[i]); return true; }
        return false;
    }
}
var frames=new FrameList(); armMissing(frames);
eq(timeline_find_frame(frames,5,false),1,"binary search");
eq(visits.join(","),"count,2,t10,1,t4,2,t10","count and live numeric reads");
check(timeline_find_frame(frames,-1,true)==-1,"before head");
check(timeline_find_frame(frames,10,false)==2 && timeline_find_frame(frames,11,false)==-1 && timeline_find_frame(frames,11,true)==2,"tail inclusion");
check(timeline_find_frame(frames,void,void)==0 && timeline_find_frame(frames,4294967301,false,99)==1,"void and i32 conversion");
check(timeline_find_frame([],0,false)==-1 && timeline_find_frame(%[],0,false)==-1,"empty and absent count");
check(timeline_find_frame(%[count:1,"0"=>%[]],0,false)==0,"absent time converts zero");
var dead=%[count:1]; invalidate dead;
check(timeline_find_frame(dead,0,false)==-1,"invalid count dispatch is ignored");
var caught=false;
visits.clear(); try { timeline_find_frame(frames,0); } catch(e) {caught=true;}
check(caught && visits.count==0,"arity before getters");
property badCount { getter { throw "count failure"; } }
caught=false; try { timeline_find_frame(%[count:&badCount],0,false); } catch(e) {caught=e=="count failure";}
check(caught,"count exception escapes");

var WIN_DARKEN2=0x123456, TIMELINE_FRAME_HEIGHT=12;
var item=%[root:%[owner:%[framePerSecond:12]],TIMELINE_FRAME_WIDTH:10,
    _singleFrameLeftColor:1,_singleFrameRightColor:2,
    _continuousFrameLeftColor:3,_continuousFrameRightColor:4,
    _tweenFrameLeftColor:5,_tweenFrameRightColor:6];
function record(name,a) { global.calls.add(name+":"+a.join(",")); }
class View {
    property oneSecondFrameBgLayer {getter {global.visits.add("one"); return 11;}}
    property halfSecondFrameBgLayer {getter {global.visits.add("half"); return 12;}}
    property fifthFrameBgLayer {getter {global.visits.add("fifth"); return 13;}}
    property normalFrameBgLayer {getter {global.visits.add("normal"); return 14;}}
    var frameLeftMarkerLayer=%[width:3,height:4,tag:"L"];
    var frameRightMarkerLayer=%[width:4,height:5,tag:"R"];
    var dashLineApp=void;
    var canvas;
    function copyRect(x,y,layer,sx,sy,w,h) { global.record("copy",[x,y,layer,sx,sy,w,h]); }
    function fillRect(x,y,w,h,c) { global.record("fill",[x,y,w,h,c]); }
    function fillGradientRectLR(x,y,w,h,l,r) { global.record("gradient",[x,y,w,h,l,r]); }
    function operateRect(x,y,layer,sx,sy,w,h) { global.record("operate",[x,y,layer.tag,sx,sy,w,h]); }
    function drawLine(app,x,y,r,b) { global.record("line",[app,x,y,r,b]); }
    function setClip(a*) { global.record("clip",a); }
    function drawText(x,y,text,c) { global.record("text",[x,y,text,c]); }
    function colorRect(x,y,w,h,c,a) { global.record("color",[x,y,w,h,c,a]); }
}
class Font {
    function getTextWidth(text) { global.record("measure",[text]); return 8; }
}
class Canvas {
    var font=new Font();
    property fontHeight {setter(v) {global.record("font",[v]);}}
}
var view=new View(); view.canvas=new Canvas();
visits.clear(); calls.clear();
eq(timeline_draw_bg(item,view,2,0,13),void,"background result");
eq(visits.join(","),"one,normal,fifth,half","lazy layer getter cache");
check(calls.count==13 && calls[0]=="copy:0,2,11,0,0,10,12" && calls[5]=="copy:50,2,13,0,0,10,12" && calls[6]=="copy:60,2,12,0,0,10,12" && calls[12]=="copy:120,2,11,0,0,10,12","background steps");
item.root.owner.framePerSecond=-12; calls.clear(); timeline_draw_bg(item,view,0,0,1);
eq(calls[0],"copy:0,0,11,0,0,10,12","negative fps is usable");
item.root.owner.framePerSecond=0; calls.clear(); timeline_draw_bg(item,view,0,1,1);
eq(calls.count,0,"zero fps without modulo");
item.root.owner.framePerSecond=12;
calls.clear(); timeline_draw_frame(item,view,2,%[type:0,time:1],2,0);
eq(calls.join(";"),"copy:10,2,14,0,0,10,12;copy:20,2,14,0,0,10,12","null frame");
calls.clear(); timeline_draw_frame(item,view,2,%[type:1,time:1],3,3);
eq(calls.join(";"),"copy:20,2,14,0,0,10,12;copy:30,2,14,0,0,10,12;gradient:10,2,9,11,-16777215,-16777214","single frame background before body");
calls.clear(); timeline_draw_frame(item,view,2,%[type:2,time:1],1,3);
eq(calls.join(";"),"fill:19,2,1,12,1193046;fill:10,13,10,1,1193046;gradient:10,2,9,11,-16777213,-16777212;operate:10,2,L,0,0,3,4","single-cell marker suppression");
calls.clear(); timeline_draw_frame(item,view,2,%[type:2,time:1],4,3);
eq(calls.join(";"),"fill:49,2,1,12,1193046;fill:10,13,40,1,1193046;gradient:10,2,39,11,-16777213,-16777212;operate:10,2,L,0,0,3,4;operate:46,2,R,0,0,4,5;font:8;measure:4;clip:25,2,10,10;gradient:10,2,39,11,-16777213,-16777212;clip:;text:26,3,4,1193046","continuous borders markers and label");
calls.clear(); timeline_draw_frame(item,view,2,%[type:3,time:1],2,0);
eq(calls.join(";"),"fill:29,2,1,12,1193046;fill:10,13,20,1,1193046;gradient:10,2,19,11,-16777211,-16777210;fill:10,7,3,1,1193046;fill:15,7,3,1,1193046;fill:20,7,3,1,1193046;fill:25,7,3,1,1193046","tween fallback dashes");
view.dashLineApp=7; calls.clear(); timeline_draw_frame(item,view,2,%[type:3,time:1],2,3);
eq(calls[calls.count-1],"line:7,20,7,20,7","tween drawLine is still called with empty marker interval");
calls.clear(); timeline_draw_frame(item,view,2,%[type:99,time:1],4,3);
eq(calls.count,0,"unknown frame type draws nothing");
// Missing/noncallable drawing members return ignored statuses; genuine errors escape.
var blank=%[fillRect:discarded,fillGradientRectLR:discarded];
timeline_draw_frame(item,blank,0,%[type:2,time:0],2,0);
timeline_draw_bg(item,%[],0,0,2);
property failedDraw {getter {throw "draw getter failure";}}
caught=false; try {timeline_draw_frame(item,%[fillRect:&failedDraw],0,%[type:2,time:0],1,0);} catch(e) {caught=e=="draw getter failure";}
check(caught,"draw property exception escapes");
property drawFunction {getter {return global.discarded;}}
timeline_draw_frame(item,%[fillRect:&drawFunction,fillGradientRectLR:&drawFunction],0,%[type:2,time:0],1,0);
class EmptyCanvas {var font=%[];}
var noFont=new EmptyCanvas(); blank.canvas=noFont;
timeline_draw_frame(item,blank,0,%[type:2,time:0],3,0);
check(typeof noFont.fontHeight=="undefined","fontHeight assignment does not ensure");

// Main timeline uses internal helpers, but captures each live item.drawFrame override.
item.layer=%[top:2];
item.frameList=%[count:2,"0"=>%[time:2],"1"=>%[time:6],"-1"=>%[time:7]];
item.selection=void;
item.drawFrame=function(v,y,f,length) {global.record("frame",[y,f.time,length]);};
var realFind=timeline_find_frame, realBg=timeline_draw_bg;
timeline_find_frame=function() {throw "replaced find";};
timeline_draw_bg=function() {throw "replaced background";};
calls.clear(); timeline_draw_timeline(item,view,0,8);
eq(calls.join(";"),"copy:0,2,11,0,0,10,12;copy:10,2,14,0,0,10,12;copy:70,2,14,0,0,10,12;frame:2,2,4;frame:2,6,0","timeline leading negative-index trailing and frames");
timeline_find_frame=realFind; timeline_draw_bg=realBg;
// Numeric selection properties are read twice per getIntValue, including void.
property selected0 {getter {global.visits.add("s0"); return 1;}}
property selected1 {getter {global.visits.add("s1"); return 3;}}
property selected2 {getter {global.visits.add("s2"); return void;}}
item.frameList=[];
item.selection=%["0"=>&selected0,"1"=>&selected1,"2"=>&selected2];
calls.clear(); visits.clear(); timeline_draw_timeline(item,view,0,0);
eq(visits.join(","),"s0,s0,s1,s1,s0,s0,s2,s2","selection HasValue then GetValue");
eq(calls.join(";"),"color:10,2,20,12,0,128","selection coordinates");
item.selection=%[]; calls.clear(); timeline_draw_timeline(item,view,0,0);
eq(calls.join(";"),"color:0,2,0,12,0,128","selection absent defaults");
"passed";
"#;
