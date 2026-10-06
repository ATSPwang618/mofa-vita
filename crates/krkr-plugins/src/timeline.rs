//! ksupport timeline.cpp. All property reads and drawing calls use the VM;
//! virtual widgets and script overrides remain part of the rendering path.
use crate::exports::Exports;
use krkr_engine::plugins::{self, Context};
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef,
    Trace, Value, value,
};

pub(crate) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    for (name, function) in [
        ("timeline_find_frame", find_frame as Function),
        ("timeline_draw_bg", draw_bg),
        ("timeline_draw_frame", draw_frame),
        ("timeline_draw_timeline", draw_timeline),
    ] {
        exports.function(cx, cx.global, name, NativeCallable::Resumable(function))?;
    }
    Ok(())
}
type Function = fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<NativeStep>;
fn text(cx: &mut NativeCx<'_>, s: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(s.encode_utf16().collect::<Vec<_>>()),
    )
}
fn int(cx: &NativeCx<'_>, v: Value) -> NativeResult<i64> {
    Ok(value::to_integer(cx.heap(), v)? as i32 as i64)
}
fn narrow(v: i64) -> i64 {
    v as i32 as i64
}
fn number(v: i64) -> Value {
    Value::Int(narrow(v))
}
fn numbers(v: &[i64]) -> Vec<Value> {
    v.iter().copied().map(number).collect()
}
// ncbind doInvoke rejects too few arguments before converting the supplied values.
fn arg(args: &[Value], index: usize) -> Value {
    args[index]
}
fn arity(args: &[Value], minimum: usize) -> NativeResult<()> {
    if args.len() < minimum {
        Err(NativeError::Missing(args.len()))
    } else {
        Ok(())
    }
}
fn accessor(value: Value) -> NativeResult<Value> {
    match value {
        Value::Obj(ObjRef {
            object: Some(id), ..
        }) => Ok(Value::Obj(ObjRef::bound(id))),
        Value::Obj(_) => Err(NativeError::Message("null timeline accessor")),
        _ => Err(NativeError::Type("an object")),
    }
}
fn global(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    Ok(Value::Obj(ObjRef::bound(plugins::global(cx)?)))
}
fn get_key(
    cx: &mut NativeCx<'_>,
    owner: Value,
    key: Value,
    continuation: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let owner = accessor(owner)?;
    let Value::Obj(reference) = owner else {
        unreachable!()
    };
    if !cx.heap().is_valid(reference.object.expect("accessor"))? {
        return Ok(tjs_bind::flow::deliver(Value::Void, continuation));
    }
    Ok(NativeStep::GetOr {
        object: owner,
        key,
        raw: false,
        fallback: Value::Void,
        continuation,
    })
}
fn get(
    cx: &mut NativeCx<'_>,
    owner: Value,
    name: &str,
    continuation: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let key = text(cx, name);
    get_key(cx, owner, key, continuation)
}
fn call(
    cx: &mut NativeCx<'_>,
    owner: Value,
    name: &str,
    arguments: Vec<Value>,
    continuation: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    Ok(NativeStep::CallMemberOr {
        object: accessor(owner)?,
        key: text(cx, name),
        arguments,
        result_needed: name == "getTextWidth",
        continuation,
    })
}
fn native(
    cx: &mut NativeCx<'_>,
    function: Function,
    arguments: Vec<Value>,
    continuation: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    // An ordinary native call frame allows an internal helper to suspend without
    // looking up a replaceable global function or reentering the Rust VM stack.
    let function = Value::Obj(
        cx.heap_mut()
            .alloc_native_function(NativeCallable::Resumable(function))
            .into(),
    );
    Ok(NativeStep::Call {
        function,
        arguments,
        continuation,
    })
}
fn done() -> NativeResult<NativeStep> {
    Ok(NativeStep::Return(Value::Void))
}
#[derive(Clone, Copy)]
enum FindPhase {
    Count,
    Last,
    LastTime,
    Current,
    CurrentTime,
    Next,
    NextTime,
}
struct Find {
    list: Value,
    frame: Value,
    next_frame: Value,
    time: i64,
    tail: bool,
    count: i64,
    begin: i64,
    end: i64,
    mid: i64,
    phase: FindPhase,
}
impl Trace for Find {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.list.trace(visit);
        self.frame.trace(visit);
        self.next_frame.trace(visit);
    }
}
impl Find {
    fn read(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        index: i64,
        phase: FindPhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        get_key(cx, self.list, number(index), self)
    }
    fn found(index: i64) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(number(index)))
    }
    fn search(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.frame = Value::Void;
        self.next_frame = Value::Void;
        if self.begin >= self.end {
            return Self::found(-1);
        }
        self.mid = (self.begin + self.end) / 2;
        let mid = self.mid;
        self.read(cx, mid, FindPhase::Current)
    }
}
impl NativeContinuation for Find {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        use FindPhase::*;
        match self.phase {
            Count => {
                self.count = int(cx, result)?;
                self.end = self.count;
                if self.count == 0 {
                    return Self::found(-1);
                }
                let last = narrow(self.count - 1);
                self.read(cx, last, Last)
            }
            Last | Current | Next => {
                if matches!(self.phase, Next) {
                    self.next_frame = result;
                } else {
                    self.frame = result;
                }
                self.phase = match self.phase {
                    Last => LastTime,
                    Current => CurrentTime,
                    _ => NextTime,
                };
                get(cx, result, "time", self)
            }
            LastTime => {
                let time = int(cx, result)?;
                if time <= self.time {
                    return Self::found(if self.tail || time == self.time {
                        self.count - 1
                    } else {
                        -1
                    });
                }
                self.search(cx)
            }
            CurrentTime => {
                if int(cx, result)? <= self.time {
                    let next = self.mid + 1;
                    self.read(cx, next, Next)
                } else {
                    self.end = self.mid;
                    self.search(cx)
                }
            }
            NextTime => {
                if self.time < int(cx, result)? {
                    return Self::found(self.mid);
                }
                self.begin = self.mid + 1;
                self.search(cx)
            }
        }
    }
}
fn find_frame(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arity(args, 3)?;
    let time = int(cx, arg(args, 1))?;
    let tail = arg(args, 2).truthy(cx.heap())?;
    let list = accessor(arg(args, 0))?;
    let work = Box::new(Find {
        list,
        frame: Value::Void,
        next_frame: Value::Void,
        time,
        tail,
        count: 0,
        begin: 0,
        end: 0,
        mid: 0,
        phase: FindPhase::Count,
    });
    get(cx, list, "count", work)
}

struct Background {
    root: Value,
    owner: Value,
    item: Value,
    view: Value,
    global: Value,
    y: i64,
    time: i64,
    end: i64,
    fps: i64,
    width: i64,
    height: i64,
    layers: [Value; 4],
    kind: usize,
    phase: u8,
}
impl Trace for Background {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.root.trace(v);
        self.owner.trace(v);
        self.item.trace(v);
        self.view.trace(v);
        self.global.trace(v);
        for layer in self.layers {
            layer.trace(v);
        }
    }
}
impl Background {
    fn copy(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.phase = 7;
        let arguments = vec![
            number(self.time * self.width),
            number(self.y),
            self.layers[self.kind],
            number(0),
            number(0),
            number(self.width),
            number(self.height),
        ];
        call(cx, self.view, "copyRect", arguments, self)
    }
}
impl NativeContinuation for Background {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.phase {
            0 => {
                self.phase = 1;
                return get(cx, self.item, "root", self);
            }
            1 => {
                self.root = accessor(result)?;
                self.phase = 2;
                return get(cx, result, "owner", self);
            }
            2 => {
                self.owner = accessor(result)?;
                self.phase = 3;
                return get(cx, result, "framePerSecond", self);
            }
            3 => {
                self.fps = int(cx, result)?;
                self.phase = 4;
                return get(cx, self.item, "TIMELINE_FRAME_WIDTH", self);
            }
            4 => {
                self.width = int(cx, result)?;
                self.phase = 5;
                return get(cx, self.global, "TIMELINE_FRAME_HEIGHT", self);
            }
            5 => self.height = int(cx, result)?,
            6 => {
                self.layers[self.kind] = result;
                return self.copy(cx);
            }
            7 => self.time += 1,
            _ => unreachable!(),
        }
        if self.time >= self.end {
            return done();
        }
        if self.fps == 0 {
            return Err(NativeError::Message(
                "timeline framePerSecond division by zero",
            ));
        }
        self.kind = if self.time % self.fps == 0 {
            0
        } else if self.time % self.fps * 2 == self.fps {
            1
        } else if self.time % 5 == 0 {
            2
        } else {
            3
        };
        if matches!(self.layers[self.kind], Value::Void) {
            let name = [
                "oneSecondFrameBgLayer",
                "halfSecondFrameBgLayer",
                "fifthFrameBgLayer",
                "normalFrameBgLayer",
            ][self.kind];
            self.phase = 6;
            get(cx, self.view, name, self)
        } else {
            self.copy(cx)
        }
    }
}
fn draw_bg(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arity(args, 5)?;
    let y = int(cx, arg(args, 2))?;
    let time = int(cx, arg(args, 3))?;
    let end = int(cx, arg(args, 4))?;
    accessor(arg(args, 0))?;
    accessor(arg(args, 1))?;
    Ok(NativeStep::Continue(Box::new(Background {
        root: Value::Void,
        owner: Value::Void,
        item: arg(args, 0),
        view: arg(args, 1),
        global: global(cx)?,
        y,
        time,
        end,
        fps: 0,
        width: 0,
        height: 0,
        layers: [Value::Void; 4],
        kind: 0,
        phase: 0,
    })))
}

#[derive(Clone, Copy)]
enum FramePhase {
    Start,
    Dark,
    Width,
    Height,
    Type,
    Time,
    SingleBackground,
    LeftColor,
    RightColor,
    RightBorder,
    BottomBorder,
    Body,
    Marker,
    MarkerWidth,
    MarkerHeight,
    MarkerDrawn,
    Dash,
    DashLeft,
    DashRight,
    DashDrawn,
    Canvas,
    FontHeight,
    Font,
    TextWidth,
    Clip,
    TextBackground,
    ResetClip,
    Done,
}
struct Frame {
    item: Value,
    view: Value,
    frame: Value,
    global: Value,
    marker: Value,
    canvas: Value,
    font: Value,
    text: Value,
    y: i64,
    time: i64,
    length: i64,
    width: i64,
    height: i64,
    dark: i64,
    kind: i64,
    left_color: i64,
    right_color: i64,
    left: bool,
    right: bool,
    marker_index: usize,
    marker_width: i64,
    dash_time: i64,
    dash_end: i64,
    text_width: i64,
    text_left: i64,
    phase: FramePhase,
}
impl Trace for Frame {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        for value in [
            self.item,
            self.view,
            self.frame,
            self.global,
            self.marker,
            self.canvas,
            self.font,
            self.text,
        ] {
            value.trace(v);
        }
    }
}
impl Frame {
    fn read(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        owner: Value,
        name: &str,
        phase: FramePhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        get(cx, owner, name, self)
    }
    fn draw(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        name: &str,
        args: Vec<Value>,
        phase: FramePhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        call(cx, self.view, name, args, self)
    }
    fn gradient(&self) -> Vec<Value> {
        numbers(&[
            self.time * self.width,
            self.y,
            if self.kind == 1 {
                self.width - 1
            } else {
                self.length * self.width - 1
            },
            self.height - 1,
            self.left_color,
            self.right_color,
        ])
    }
    fn colors(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let name = match self.kind {
            1 => "_singleFrameLeftColor",
            2 => "_continuousFrameLeftColor",
            _ => "_tweenFrameLeftColor",
        };
        let item = self.item;
        self.read(cx, item, name, FramePhase::LeftColor)
    }
    fn marker(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        while self.marker_index < 2 {
            let enabled = if self.marker_index == 0 {
                self.left
            } else {
                self.right
            };
            if enabled {
                let view = self.view;
                let name = if self.marker_index == 0 {
                    "frameLeftMarkerLayer"
                } else {
                    "frameRightMarkerLayer"
                };
                return self.read(cx, view, name, FramePhase::Marker);
            }
            self.marker_index += 1;
        }
        if self.kind == 3 {
            let view = self.view;
            self.read(cx, view, "dashLineApp", FramePhase::Dash)
        } else {
            self.label(cx)
        }
    }
    fn label(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.length <= 2 {
            return done();
        }
        let view = self.view;
        self.read(cx, view, "canvas", FramePhase::Canvas)
    }
    fn dash(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.dash_time >= self.dash_end {
            return self.label(cx);
        }
        let args = numbers(&[
            self.dash_time * self.width,
            self.y + self.height / 2 - 1,
            self.width / 2 - 2,
            1,
            self.dark,
        ]);
        self.draw(cx, "fillRect", args, FramePhase::DashLeft)
    }
}
impl NativeContinuation for Frame {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        use FramePhase::*;
        match self.phase {
            Start => {
                let global = self.global;
                self.read(cx, global, "WIN_DARKEN2", Dark)
            }
            Dark => {
                self.dark = int(cx, result)?;
                let item = self.item;
                self.read(cx, item, "TIMELINE_FRAME_WIDTH", Width)
            }
            Width => {
                self.width = int(cx, result)?;
                let global = self.global;
                self.read(cx, global, "TIMELINE_FRAME_HEIGHT", Height)
            }
            Height => {
                self.height = int(cx, result)?;
                let frame = self.frame;
                self.read(cx, frame, "type", Type)
            }
            Type => {
                self.kind = int(cx, result)?;
                let frame = self.frame;
                self.read(cx, frame, "time", Time)
            }
            Time => {
                self.time = int(cx, result)?;
                match self.kind {
                    0 | 1 => {
                        let args = vec![
                            self.item,
                            self.view,
                            number(self.y),
                            number(narrow(self.time + i64::from(self.kind == 1))),
                            number(self.time + self.length),
                        ];
                        self.phase = if self.kind == 0 {
                            FramePhase::Done
                        } else {
                            SingleBackground
                        };
                        native(cx, draw_bg, args, self)
                    }
                    2 | 3 => self.colors(cx),
                    _ => done(),
                }
            }
            SingleBackground => self.colors(cx),
            LeftColor => {
                self.left_color = narrow(int(cx, result)? | 0xff000000);
                let name = match self.kind {
                    1 => "_singleFrameRightColor",
                    2 => "_continuousFrameRightColor",
                    _ => "_tweenFrameRightColor",
                };
                let item = self.item;
                self.read(cx, item, name, RightColor)
            }
            RightColor => {
                self.right_color = narrow(int(cx, result)? | 0xff000000);
                if self.kind == 1 {
                    let args = self.gradient();
                    return self.draw(cx, "fillGradientRectLR", args, FramePhase::Done);
                }
                let args = numbers(&[
                    narrow(self.time + self.length) * self.width - 1,
                    self.y,
                    1,
                    self.height,
                    self.dark,
                ]);
                self.draw(cx, "fillRect", args, RightBorder)
            }
            RightBorder => {
                let args = numbers(&[
                    self.time * self.width,
                    self.y + self.height - 1,
                    self.length * self.width,
                    1,
                    self.dark,
                ]);
                self.draw(cx, "fillRect", args, BottomBorder)
            }
            BottomBorder => {
                let args = self.gradient();
                self.draw(cx, "fillGradientRectLR", args, Body)
            }
            Body => self.marker(cx),
            Marker => {
                self.marker = result;
                self.read(cx, result, "width", MarkerWidth)
            }
            MarkerWidth => {
                self.marker_width = int(cx, result)?;
                let marker = self.marker;
                self.read(cx, marker, "height", MarkerHeight)
            }
            MarkerHeight => {
                let x = if self.marker_index == 0 {
                    self.time * self.width
                } else {
                    narrow(self.time + self.length) * self.width - self.marker_width
                };
                let args = vec![
                    number(x),
                    number(self.y),
                    self.marker,
                    number(0),
                    number(0),
                    number(self.marker_width),
                    number(int(cx, result)?),
                ];
                self.draw(cx, "operateRect", args, MarkerDrawn)
            }
            MarkerDrawn => {
                self.marker = Value::Void;
                self.marker_index += 1;
                self.marker(cx)
            }
            Dash => {
                self.dash_time = narrow(self.time + i64::from(self.left));
                self.dash_end = narrow(self.time + self.length - i64::from(self.right));
                if matches!(result, Value::Void) {
                    self.dash(cx)
                } else {
                    let args = vec![
                        result,
                        number(self.dash_time * self.width),
                        number(self.y + self.height / 2 - 1),
                        number(self.dash_end * self.width),
                        number(self.y + self.height / 2 - 1),
                    ];
                    self.draw(cx, "drawLine", args, DashDrawn)
                }
            }
            DashLeft => {
                let args = numbers(&[
                    self.dash_time * self.width + self.width / 2,
                    self.y + self.height / 2 - 1,
                    self.width / 2 - 2,
                    1,
                    self.dark,
                ]);
                self.draw(cx, "fillRect", args, DashRight)
            }
            DashRight => {
                self.dash_time += 1;
                self.dash(cx)
            }
            DashDrawn => self.label(cx),
            Canvas => {
                self.canvas = result;
                self.phase = FontHeight;
                Ok(NativeStep::SetExisting {
                    object: accessor(result)?,
                    key: text(cx, "fontHeight"),
                    value: number(self.height - 4),
                    continuation: self,
                })
            }
            FontHeight => {
                let canvas = self.canvas;
                self.read(cx, canvas, "font", Font)
            }
            Font => {
                self.font = result;
                self.text = text(cx, &self.length.to_string());
                self.phase = TextWidth;
                call(cx, result, "getTextWidth", vec![self.text], self)
            }
            TextWidth => {
                self.text_width = int(cx, result)?;
                self.text_left = narrow(
                    narrow(self.time * self.width)
                        + narrow(narrow(self.length * self.width) - self.text_width) / 2,
                );
                let args = numbers(&[
                    self.text_left - 1,
                    self.y,
                    self.text_width + 2,
                    self.height - 2,
                ]);
                self.draw(cx, "setClip", args, Clip)
            }
            Clip => {
                let args = self.gradient();
                self.draw(cx, "fillGradientRectLR", args, TextBackground)
            }
            TextBackground => self.draw(cx, "setClip", vec![], ResetClip),
            ResetClip => {
                let args = vec![
                    number(self.text_left),
                    number(self.y + 1),
                    self.text,
                    number(self.dark & 0xffffff),
                ];
                self.draw(cx, "drawText", args, FramePhase::Done)
            }
            FramePhase::Done => done(),
        }
    }
}
fn draw_frame(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arity(args, 6)?;
    let y = int(cx, arg(args, 2))?;
    let length = int(cx, arg(args, 4))?;
    let mask = int(cx, arg(args, 5))?;
    let left = mask & 1 != 0;
    let right = mask & 2 != 0 && !(left && length == 1);
    accessor(arg(args, 0))?;
    accessor(arg(args, 1))?;
    accessor(arg(args, 3))?;
    Ok(NativeStep::Continue(Box::new(Frame {
        item: arg(args, 0),
        view: arg(args, 1),
        frame: arg(args, 3),
        global: global(cx)?,
        marker: Value::Void,
        canvas: Value::Void,
        font: Value::Void,
        text: Value::Void,
        y,
        time: 0,
        length,
        width: 0,
        height: 0,
        dark: 0,
        kind: 0,
        left_color: 0,
        right_color: 0,
        left,
        right,
        marker_index: 0,
        marker_width: 0,
        dash_time: 0,
        dash_end: 0,
        text_width: 0,
        text_left: 0,
        phase: FramePhase::Start,
    })))
}

// ncbPropAccessor::getIntValue first probes with MEMBERMUSTEXIST, then
// reads again. Even an existing void performs both getter/missing dispatches.
struct IntValue {
    source: Value,
    key: Value,
    absent: Value,
    reading: bool,
    next: Box<dyn NativeContinuation>,
}
impl Trace for IntValue {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.source.trace(visit);
        self.key.trace(visit);
        self.absent.trace(visit);
        self.next.trace(visit);
    }
}
impl NativeContinuation for IntValue {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.reading {
            let value = number(int(cx, result)?);
            return self.next.resume(cx, value);
        }
        if value::strict_equal(cx.heap(), result, self.absent)? {
            return self.next.resume(cx, number(0));
        }
        self.reading = true;
        get_key(cx, self.source, self.key, self)
    }
}
fn get_int_value(
    cx: &mut NativeCx<'_>,
    source: Value,
    index: i64,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let source = accessor(source)?;
    let Value::Obj(reference) = source else {
        unreachable!()
    };
    if !cx.heap().is_valid(reference.object.expect("accessor"))? {
        return Ok(tjs_bind::flow::deliver(number(0), next));
    }
    let key = number(index);
    let absent = Value::Obj(ObjRef::bound(cx.heap_mut().alloc_dictionary()));
    Ok(NativeStep::GetRequiredOr {
        object: source,
        key,
        fallback: absent,
        continuation: Box::new(IntValue {
            source,
            key,
            absent,
            reading: false,
            next,
        }),
    })
}

#[derive(Clone, Copy)]
enum TimelinePhase {
    Start,
    Layer,
    Top,
    List,
    Count,
    From,
    To,
    FirstFrame,
    FirstTime,
    LeadingBg,
    LastFrame,
    LastTime,
    TrailingBg,
    Frame,
    Head,
    NextFrame,
    Tail,
    Drawn,
    Selection,
    Width,
    Height,
    SelectionValue,
    Done,
}
struct Timeline {
    item: Value,
    view: Value,
    global: Value,
    layer: Value,
    list: Value,
    frame: Value,
    next_frame: Value,
    selection: Value,
    y: i64,
    from: i64,
    to: i64,
    count: i64,
    first: i64,
    last: i64,
    index: i64,
    head: i64,
    width: i64,
    height: i64,
    selected: [i64; 4],
    selection_index: usize,
    phase: TimelinePhase,
}
impl Trace for Timeline {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for value in [
            self.item,
            self.view,
            self.global,
            self.layer,
            self.list,
            self.frame,
            self.next_frame,
            self.selection,
        ] {
            value.trace(visit);
        }
    }
}
impl Timeline {
    fn read(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        owner: Value,
        name: &str,
        phase: TimelinePhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        get(cx, owner, name, self)
    }
    fn entry(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        index: i64,
        phase: TimelinePhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        get_key(cx, self.list, number(index), self)
    }
    fn background(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        from: i64,
        to: i64,
        phase: TimelinePhase,
    ) -> NativeResult<NativeStep> {
        self.phase = phase;
        let args = vec![
            self.item,
            self.view,
            number(self.y),
            number(from),
            number(to),
        ];
        native(cx, draw_bg, args, self)
    }
    fn trailing(self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.last == self.count - 1 {
            // This is literally -1, not count-1: object-backed lists can observe it.
            self.entry(cx, -1, TimelinePhase::LastFrame)
        } else {
            self.next(cx)
        }
    }
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        self.frame = Value::Void;
        self.next_frame = Value::Void;
        if self.index == -1 {
            self.index += 1;
        }
        if self.index > self.last {
            let item = self.item;
            return self.read(cx, item, "selection", TimelinePhase::Selection);
        }
        let index = self.index;
        self.entry(cx, index, TimelinePhase::Frame)
    }
    fn draw(mut self: Box<Self>, cx: &mut NativeCx<'_>, tail: i64) -> NativeResult<NativeStep> {
        // nextFrame's block ends before the drawFrame callback.
        self.next_frame = Value::Void;
        self.index += 1;
        if tail < self.from || self.head > self.to {
            return self.next(cx);
        }
        self.phase = TimelinePhase::Drawn;
        let args = vec![
            self.view,
            number(self.y),
            self.frame,
            number(tail - self.head),
        ];
        call(cx, self.item, "drawFrame", args, self)
    }
    fn selected(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.selection_index < 4 {
            // Source order; C++ does not specify relative argument evaluation order.
            let index = [0, 1, 0, 2][self.selection_index];
            self.phase = TimelinePhase::SelectionValue;
            return get_int_value(cx, self.selection, index, self);
        }
        self.phase = TimelinePhase::Done;
        let args = numbers(&[
            self.selected[0] * self.width,
            self.y,
            narrow(self.selected[1] - self.selected[2]) * self.width,
            self.height,
            if self.selected[3] != 0 { 0xff0000 } else { 0 },
            128,
        ]);
        call(cx, self.view, "colorRect", args, self)
    }
}
impl NativeContinuation for Timeline {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        use TimelinePhase::*;
        match self.phase {
            Start => {
                let item = self.item;
                self.read(cx, item, "layer", Layer)
            }
            Layer => {
                self.layer = result;
                self.read(cx, result, "top", Top)
            }
            Top => {
                self.y = int(cx, result)?;
                let item = self.item;
                self.read(cx, item, "frameList", List)
            }
            List => {
                self.list = result;
                self.read(cx, result, "count", Count)
            }
            Count => {
                self.count = int(cx, result)?;
                self.phase = From;
                let args = vec![self.list, number(self.from), number(1)];
                native(cx, find_frame, args, self)
            }
            From => {
                self.first = int(cx, result)?;
                self.phase = To;
                let args = vec![self.list, number(self.to), number(1)];
                native(cx, find_frame, args, self)
            }
            To => {
                self.last = int(cx, result)?;
                self.index = self.first;
                if self.first < 0 && self.last < 0 {
                    let (from, to) = (self.from, self.to);
                    self.background(cx, from, to, TrailingBg)
                } else if self.first < 0 {
                    self.entry(cx, 0, FirstFrame)
                } else {
                    self.trailing(cx)
                }
            }
            FirstFrame | LastFrame | Frame | NextFrame => {
                self.phase = match self.phase {
                    FirstFrame => FirstTime,
                    LastFrame => LastTime,
                    Frame => Head,
                    _ => Tail,
                };
                if matches!(self.phase, Tail) {
                    self.next_frame = result;
                } else {
                    self.frame = result;
                }
                get(cx, result, "time", self)
            }
            FirstTime => {
                let from = self.from;
                self.background(cx, from, int(cx, result)?, LeadingBg)
            }
            LeadingBg => {
                self.frame = Value::Void;
                self.trailing(cx)
            }
            LastTime => {
                let to = self.to;
                self.background(cx, int(cx, result)?, to, TrailingBg)
            }
            TrailingBg | Drawn => self.next(cx),
            Head => {
                self.head = int(cx, result)?;
                if self.index < self.count - 1 {
                    let next = self.index + 1;
                    self.entry(cx, next, NextFrame)
                } else {
                    let head = self.head;
                    self.draw(cx, head)
                }
            }
            Tail => self.draw(cx, int(cx, result)?),
            Selection => {
                if matches!(result, Value::Void) {
                    return done();
                }
                self.selection = result;
                let item = self.item;
                self.read(cx, item, "TIMELINE_FRAME_WIDTH", Width)
            }
            Width => {
                self.width = int(cx, result)?;
                let global = self.global;
                self.read(cx, global, "TIMELINE_FRAME_HEIGHT", Height)
            }
            Height => {
                self.height = int(cx, result)?;
                self.selected(cx)
            }
            SelectionValue => {
                self.selected[self.selection_index] = int(cx, result)?;
                self.selection_index += 1;
                self.selected(cx)
            }
            TimelinePhase::Done => done(),
        }
    }
}
fn draw_timeline(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arity(args, 4)?;
    let from = int(cx, arg(args, 2))?;
    let to = int(cx, arg(args, 3))?;
    accessor(arg(args, 0))?;
    accessor(arg(args, 1))?;
    Ok(NativeStep::Continue(Box::new(Timeline {
        item: arg(args, 0),
        view: arg(args, 1),
        global: global(cx)?,
        layer: Value::Void,
        list: Value::Void,
        frame: Value::Void,
        next_frame: Value::Void,
        selection: Value::Void,
        y: 0,
        from,
        to,
        count: 0,
        first: 0,
        last: 0,
        index: 0,
        head: 0,
        width: 0,
        height: 0,
        selected: [0; 4],
        selection_index: 0,
        phase: TimelinePhase::Start,
    })))
}
